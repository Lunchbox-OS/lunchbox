//! The file routes (issue #195).
//!
//! ```text
//! GET    /api/v1/files/roots     the places a caller may browse
//! GET    /api/v1/files/list      one directory
//! GET    /api/v1/files/content   download            (HEAD too)
//! PUT    /api/v1/files/content   upload or replace
//! POST   /api/v1/files/dir       create a directory
//! POST   /api/v1/files/move      rename within one root
//! DELETE /api/v1/files/entry     delete a file or directory
//! ```
//!
//! All of them sit inside the same `require_auth` layer as the rest of
//! `/api/v1`, so everything issue #156 built applies unchanged: a session
//! cookie or a bearer token, the `Origin`-vs-`Host` check on writes, the
//! lockout, TLS, revocation from the sessions list.
//!
//! ## `root` + `path`, never an absolute path
//!
//! The caller names one of the roots the server enumerated and a path inside
//! it. A closed set the server offers cannot express a location it did not
//! offer; a validated absolute path is one missed call site away from serving
//! `/etc`. Both travel as **query parameters** rather than as path segments —
//! a wildcard segment is percent-decoded by the router before any check sees
//! it, which is the classic way `%2e%2e%2f` becomes `../` one layer too early.
//!
//! ## Downloads are always attachments
//!
//! An uploaded `.html` served inline from this origin would run script *on the
//! management origin*, where `fetch('/api/v1/rpc')` carries the
//! administrator's cookie — `HttpOnly` does not help, because such a script
//! never reads the cookie, it is merely sent with it. `Content-Disposition:
//! attachment` plus `X-Content-Type-Options: nosniff` plus a fixed
//! `application/octet-stream` is what stops that, and no future preview
//! feature may weaken any of the three.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::{
    Json,
    body::Body,
    extract::{Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::json;
use tokio_stream::StreamExt;

use crate::files::{
    DEFAULT_LIMIT, FileError, Granularity, Located, MAX_LIMIT, Validator, list_dir, validator_of,
};
use crate::state::AppState;

/// A root and a path inside it. Every route speaks this.
#[derive(Debug, Deserialize)]
pub struct Target {
    root: String,
    #[serde(default)]
    path: String,
    /// A resumable upload's identity, when this is one. See [`upload`].
    #[serde(default)]
    upload: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    root: String,
    #[serde(default)]
    path: String,
    limit: Option<usize>,
    cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DeleteQuery {
    root: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    recursive: bool,
    /// Name the entry by its bytes instead of by `path`'s last component.
    ///
    /// When this is present `path` names the **containing directory** and the
    /// handle names one entry inside it. It exists for the entries whose names
    /// are not valid UTF-8: those cannot ride in a query string at all, so
    /// without it a file this API *lists* — and flags as unusable — could
    /// never be got rid of, on a device whose whole premise is that there is
    /// no shell to do it from.
    handle: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DirRequest {
    root: String,
    path: String,
}

#[derive(Debug, Deserialize)]
pub struct MoveRequest {
    root: String,
    from: String,
    to: String,
    #[serde(default)]
    overwrite: bool,
}

// ---------------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------------

/// Everywhere this device will let a caller browse, and the upload limits.
pub async fn roots(State(state): State<AppState>) -> Response {
    let Some(files) = state.file_manager.clone() else {
        return disabled();
    };
    let settings = files.settings();
    let roots = blocking(move || files.roots()).await;
    let roots = match roots {
        Ok(roots) => roots,
        Err(e) => return e.into_response(),
    };
    Json(json!({
        "roots": roots,
        "limits": {
            "max_upload_bytes": settings.max_upload_bytes,
            "free_space_floor_bytes": settings.free_space_floor_bytes,
        },
    }))
    .into_response()
}

/// One directory, sorted and paginated.
pub async fn list(State(state): State<AppState>, Query(q): Query<ListQuery>) -> Response {
    let Some(files) = state.file_manager.clone() else {
        return disabled();
    };
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let result = blocking(move || {
        let located = files.locate(&q.root, &q.path)?;
        let dir = located.follow(files.denied())?;
        // Before the listing, so a part file collected here is not also
        // reported as being there.
        //
        // Reading a directory is the other moment a stale part file can be
        // noticed — and, unlike an upload, it is the moment somebody is
        // actually *looking* at the folder the stray bytes are in. The two
        // upload call sites only ever sweep the directory an upload is landing
        // in, so a transfer abandoned into a folder nobody uploads to again
        // would otherwise keep its bytes for good.
        //
        // The cost is one extra `getdents` pass. It is not the second walk it
        // looks like: the sweep `stat`s only the entries whose names match
        // `.*.part`, which is approximately none of them, while the listing
        // below `stat`s every single one — so against a ROM directory this is
        // noise beside the work already being done.
        if located.root.writable {
            sweep_stale_parts(&dir);
        }
        list_dir(
            &located.root,
            &q.path,
            &dir,
            files.denied(),
            limit,
            q.cursor.as_deref(),
        )
    })
    .await;
    match result {
        Ok(Ok(listing)) => Json(listing).into_response(),
        Ok(Err(e)) | Err(e) => e.into_response(),
    }
}

/// Download.
pub async fn download(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<Target>,
) -> Response {
    let Some(files) = state.file_manager.clone() else {
        return disabled();
    };
    let opened = blocking(move || {
        let located = files.locate(&q.root, &q.path)?;
        let granularity = located.root.granularity;
        let (file, meta, name) = open_for_read(&located, files.denied())?;
        Ok::<_, FileError>((file, meta, name, granularity))
    })
    .await;
    let (file, meta, name, granularity) = match opened {
        Ok(Ok(v)) => v,
        Ok(Err(e)) | Err(e) => return e.into_response(),
    };

    let validator = validator_of(&meta, granularity);
    let etag = validator.value.clone();
    let len = meta.len();

    // `If-Match`, which is what **Firefox** sends when it resumes an
    // interrupted download — not `If-Range`, which is what the specification's
    // resumption story is usually told with and what Chromium sends. Ignoring
    // it is not a missing nicety: a resume whose validator no longer matches
    // would be answered with bytes from the *new* file at the old offset, and
    // the browser would stitch the two halves into a file that matches
    // neither version and report it as a successful download. Measured, not
    // theorised — see
    // `docs/ai/history/2026-09-14 001 browser-download-resume (#195).md`.
    if let Some(if_match) = header_str(&headers, header::IF_MATCH) {
        // Strong comparison, which is what `If-Match` is defined to use — and
        // on a filesystem whose timestamps are too coarse to promise the tag
        // changes with the content, nothing compares strongly. That is the
        // whole protection on a FAT drive: a resume inside the two-second
        // window is refused rather than served bytes from a different file.
        let matches = if_match.trim() == "*"
            || if_match
                .split(',')
                .any(|candidate| validator.strongly_matches(candidate));
        if !matches {
            return FileError::PreconditionFailed(
                "that file has changed since it was first read; download it again".into(),
            )
            .into_response();
        }
    }

    // A cached copy is still good: the tag is derived from size and mtime, so
    // a file rewritten byte-identically in the same nanosecond is the only way
    // to fool it, and that is not a thing that happens to a book.
    // Not answered from a weak validator: "you already have this" is exactly
    // the claim a coarse filesystem cannot support.
    if let Some(inm) = header_str(&headers, header::IF_NONE_MATCH)
        && !validator.weak
        && (inm.trim() == "*" || unquote(inm) == etag)
    {
        return (
            StatusCode::NOT_MODIFIED,
            download_headers(&name, &validator, None),
        )
            .into_response();
    }

    // `If-Range`, which is what makes a *resumed* download safe: a browser
    // continuing an interrupted one sends the validator it started with, and a
    // file that changed since must be sent whole rather than stitched onto
    // bytes from the previous version. A date form never matches, because the
    // only validator this API emits is an `ETag`.
    // A weak validator can never assemble a range — the specification says so,
    // and it is the reason Chrome restarts instead of stitching when this
    // answers with one.
    let range_is_stale = header_str(&headers, header::IF_RANGE)
        .is_some_and(|value| !validator.strongly_matches(value));
    let range = match header_str(&headers, header::RANGE)
        .filter(|_| !range_is_stale)
        .map(|r| parse_range(r, len))
    {
        Some(Ok(range)) => Some(range),
        Some(Err(e)) => {
            return (
                StatusCode::RANGE_NOT_SATISFIABLE,
                [(header::CONTENT_RANGE, format!("bytes */{len}"))],
                Json(json!({ "error": "range_not_satisfiable", "message": e })),
            )
                .into_response();
        }
        None => None,
    };

    let mut file = tokio::fs::File::from_std(file);
    let (status, start, count) = match range {
        Some((start, end)) => (StatusCode::PARTIAL_CONTENT, start, end - start + 1),
        None => (StatusCode::OK, 0, len),
    };
    if start > 0
        && tokio::io::AsyncSeekExt::seek(&mut file, std::io::SeekFrom::Start(start))
            .await
            .is_err()
    {
        return FileError::Internal("could not seek that file".into()).into_response();
    }

    let stream = tokio_util::io::ReaderStream::new(tokio::io::AsyncReadExt::take(file, count));
    let content_range = range.map(|(start, end)| format!("bytes {start}-{end}/{len}"));
    let mut response = (
        status,
        download_headers(&name, &validator, content_range),
        Body::from_stream(stream),
    )
        .into_response();
    response
        .headers_mut()
        .insert(header::CONTENT_LENGTH, count.into());
    response
}

/// Upload, or replace.
pub async fn upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<Target>,
    body: Body,
) -> Response {
    let Some(files) = state.file_manager.clone() else {
        return disabled();
    };
    let settings = files.settings();

    let precondition = match Precondition::from_headers(&headers, true) {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };

    // A resumable upload: one chunk of a file, identified by a token the
    // client chose, appended to a part file that survives the connection
    // dying. See [`resumable_upload`].
    if let Some(token) = q.upload.clone() {
        return resumable_upload(files, &headers, q, token, precondition, body).await;
    }

    // Everything that can be decided before a byte is read, decided before a
    // byte is read: a 4 GiB upload refused after it has been transferred is a
    // refusal nobody thanks you for.
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    let files_for_prep = files.clone();
    let q_root = q.root.clone();
    let q_path = q.path.clone();
    let prepared = blocking(move || {
        let located = files_for_prep.locate(&q_root, &q_path)?;
        let floor_applies = located
            .path
            .parent()
            .and_then(|parent| std::fs::metadata(parent).ok())
            .map(|meta| files_for_prep.floor_applies(&meta))
            .unwrap_or(true);
        prepare_write(&located, precondition, declared, &settings, floor_applies)
    })
    .await;
    let plan = match prepared {
        Ok(Ok(plan)) => plan,
        Ok(Err(e)) | Err(e) => return e.into_response(),
    };

    match stream_to_temp(body, &plan).await {
        Ok(size) => {
            let finished = blocking(move || finish_write(plan, size)).await;
            match finished {
                Ok(Ok((created, validator, size))) => (
                    if created {
                        StatusCode::CREATED
                    } else {
                        StatusCode::OK
                    },
                    // The header carries the wire form, weakness and all: a
                    // file just written to a FAT drive is inside the tick its
                    // tag cannot see past, and saying otherwise here would
                    // hand the caller a validator it must not trust. The JSON
                    // field stays the bare value, matching the listing.
                    [(header::ETAG, validator.header())],
                    Json(json!({ "path": q.path, "size": size, "etag": validator.value })),
                )
                    .into_response(),
                Ok(Err(e)) | Err(e) => e.into_response(),
            }
        }
        Err(e) => {
            // The half-written temp file goes with the failure. Nothing else
            // would ever remove it under its own name.
            let _ = std::fs::remove_file(&plan.temp);
            e.into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// Resumable uploads
//
// A device on repurposed hardware has the wifi chip it came with, and a drop
// at 95% of a 4 GiB video should not cost 4 GiB. So an upload can arrive as a
// series of chunks that append to the same part file, and a client that lost
// the connection asks where it got to and carries on from there.
//
// The state *is* the part file — there is no session table, nothing to expire
// and nothing to lose in a restart. What identifies it is a token the client
// chose, which is also part of a filename, and is therefore validated the way
// every other caller-supplied name here is.
// ---------------------------------------------------------------------------

/// `bytes X-Y/Z`, the header a chunk carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ChunkRange {
    start: u64,
    /// Inclusive, as the header spells it.
    end: u64,
    total: u64,
}

impl ChunkRange {
    fn len(&self) -> u64 {
        self.end - self.start + 1
    }

    fn completes(&self) -> bool {
        self.end + 1 == self.total
    }
}

fn parse_content_range(value: &str) -> Result<ChunkRange, FileError> {
    let bad = || FileError::BadRequest("Content-Range must be `bytes X-Y/Z`".into());
    let spec = value.trim().strip_prefix("bytes ").ok_or_else(bad)?;
    let (range, total) = spec.split_once('/').ok_or_else(bad)?;
    let (start, end) = range.split_once('-').ok_or_else(bad)?;
    let start: u64 = start.trim().parse().map_err(|_| bad())?;
    let end: u64 = end.trim().parse().map_err(|_| bad())?;
    let total: u64 = total.trim().parse().map_err(|_| bad())?;
    if end < start || end >= total {
        return Err(FileError::BadRequest(
            "that Content-Range does not describe a piece of the file".into(),
        ));
    }
    Ok(ChunkRange { start, end, total })
}

/// The token names a file on disk, so it is checked like every other name.
///
/// The alphabet is deliberately narrow — no dot, no slash, no separator of any
/// kind — so the part file's name cannot be steered anywhere by choosing a
/// clever token.
fn validate_token(token: &str) -> Result<&str, FileError> {
    let ok = token.len() >= 8
        && token.len() <= 64
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if ok {
        Ok(token)
    } else {
        Err(FileError::BadRequest(
            "an upload token is 8 to 64 characters of letters, digits, - and _".into(),
        ))
    }
}

/// Where the chunks of one resumable upload accumulate.
///
/// Beside the file it is becoming, and dotted, for the same reasons the
/// one-shot path's temp file is: the rename that publishes it is atomic only
/// within a directory, and a listing hides it by default.
fn part_path(target: &Path, token: &str) -> Result<PathBuf, FileError> {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .ok_or_else(|| FileError::BadRequest("name a file to write".into()))?;
    let parent = target
        .parent()
        .ok_or_else(|| FileError::BadRequest("name a file to write".into()))?;
    Ok(parent.join(format!(".{name}.{token}.part")))
}

/// How much of a resumable upload this device already holds.
fn part_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

/// `GET /api/v1/files/upload` — where to carry on from.
///
/// Answers `0` for an upload this device has never seen, so a client that
/// starts and a client that resumes ask the same question and read the same
/// answer.
pub async fn upload_offset(State(state): State<AppState>, Query(q): Query<Target>) -> Response {
    let Some(files) = state.file_manager.clone() else {
        return disabled();
    };
    let Some(token) = q.upload.clone() else {
        return FileError::BadRequest("name the upload".into()).into_response();
    };
    let result = blocking(move || {
        let token = validate_token(&token)?;
        let located = files.locate(&q.root, &q.path)?;
        let part = part_path(&located.path, token)?;
        Ok::<u64, FileError>(part_len(&part))
    })
    .await;
    match result {
        Ok(Ok(offset)) => Json(json!({ "offset": offset })).into_response(),
        Ok(Err(e)) | Err(e) => e.into_response(),
    }
}

/// `DELETE /api/v1/files/upload` — give up on one, and take its bytes with it.
///
/// A later upload or listing in that folder would collect it a day on; a
/// cancel that leaves gigabytes on a
/// small disk until tomorrow is not a cancel.
pub async fn abandon_upload(State(state): State<AppState>, Query(q): Query<Target>) -> Response {
    let Some(files) = state.file_manager.clone() else {
        return disabled();
    };
    let Some(token) = q.upload.clone() else {
        return FileError::BadRequest("name the upload".into()).into_response();
    };
    let result = blocking(move || {
        let token = validate_token(&token)?;
        let located = files.locate(&q.root, &q.path)?;
        let part = part_path(&located.path, token)?;
        match std::fs::remove_file(&part) {
            Ok(()) => Ok(()),
            // Already gone is the outcome the caller wanted.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(FileError::from_io(&e, "that upload")),
        }
    })
    .await;
    match result {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(e)) | Err(e) => e.into_response(),
    }
}

/// One chunk of a resumable upload.
///
/// The precondition is evaluated twice, and both times matter: at the first
/// chunk, so "create, do not replace" fails before anything is transferred,
/// and again at the rename, so a file that appeared while the upload was in
/// flight is not silently overwritten by it.
async fn resumable_upload(
    files: Arc<crate::files::FileService>,
    headers: &HeaderMap,
    q: Target,
    token: String,
    precondition: Precondition,
    body: Body,
) -> Response {
    let settings = files.settings();
    let Some(range) = header_str(headers, header::CONTENT_RANGE) else {
        return FileError::BadRequest(
            "a resumable upload sends Content-Range: bytes X-Y/Z on every chunk".into(),
        )
        .into_response();
    };
    let range = match parse_content_range(range) {
        Ok(range) => range,
        Err(e) => return e.into_response(),
    };

    let prep_files = files.clone();
    let (root, path) = (q.root.clone(), q.path.clone());
    let prepared = blocking(move || {
        let token = validate_token(&token)?.to_string();
        let located = prep_files.locate(&root, &path)?;
        let floor_applies = located
            .path
            .parent()
            .and_then(|parent| std::fs::metadata(parent).ok())
            .map(|meta| prep_files.floor_applies(&meta))
            .unwrap_or(true);
        prepare_chunk(
            &located,
            &token,
            range,
            precondition,
            &settings,
            floor_applies,
        )
    })
    .await;
    let plan = match prepared {
        Ok(Ok(plan)) => plan,
        Ok(Err(e)) | Err(e) => return e.into_response(),
    };

    match append_chunk(body, &plan, range).await {
        Ok(()) => {}
        Err(e) => {
            // The part file is *kept*: it is the resumption state, and this is
            // exactly the failure it exists for. `DELETE /files/upload` is what
            // removes it deliberately; failing that, the sweep collects it once
            // it is a day old, the next time anybody uploads into this folder
            // or lists it.
            return e.into_response();
        }
    }

    if !range.completes() {
        return (
            StatusCode::NO_CONTENT,
            [(UPLOAD_OFFSET, (range.end + 1).to_string())],
        )
            .into_response();
    }

    let finished = blocking(move || finish_chunked(plan)).await;
    match finished {
        Ok(Ok((created, validator, size))) => (
            if created {
                StatusCode::CREATED
            } else {
                StatusCode::OK
            },
            [(header::ETAG, validator.header())],
            Json(json!({ "path": q.path, "size": size, "etag": validator.value })),
        )
            .into_response(),
        Ok(Err(e)) | Err(e) => e.into_response(),
    }
}

/// `Upload-Offset`, the one header this API invents.
const UPLOAD_OFFSET: header::HeaderName = header::HeaderName::from_static("upload-offset");

struct ChunkPlan {
    target: PathBuf,
    part: PathBuf,
    precondition: Precondition,
    /// The destination filesystem's, for the precondition at the rename.
    granularity: Granularity,
    /// Whether the target existed when the upload began.
    created: bool,
    max_bytes: u64,
    floor: u64,
    free: Option<u64>,
    total: u64,
}

fn prepare_chunk(
    located: &Located,
    token: &str,
    range: ChunkRange,
    precondition: Precondition,
    settings: &shepherd_config::FileManagerConfig,
    floor_applies: bool,
) -> Result<ChunkPlan, FileError> {
    if !located.root.writable {
        return Err(FileError::Forbidden("that place is read-only".into()));
    }
    let Some(parent) = located.path.parent() else {
        return Err(FileError::BadRequest("name a file to write".into()));
    };
    let parent_meta =
        std::fs::metadata(parent).map_err(|e| FileError::from_io(&e, "that folder"))?;
    if !parent_meta.is_dir() {
        return Err(FileError::Conflict("that is a file, not a folder".into()));
    }

    let existing = match std::fs::symlink_metadata(&located.path) {
        Ok(meta) => Some(meta),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(FileError::from_io(&e, "that file")),
    };
    if let Some(meta) = &existing {
        if meta.is_dir() {
            return Err(FileError::Conflict("that name belongs to a folder".into()));
        }
        if meta.file_type().is_symlink() {
            return Err(FileError::Conflict(
                "that name is a link; delete it first".into(),
            ));
        }
    }
    // At the first chunk, so a refusal costs nothing. Checked again at the
    // rename, because the answer can change while an upload is in flight.
    if range.start == 0 {
        precondition.check(existing.as_ref(), located.root.granularity)?;
    }

    if settings.max_upload_bytes > 0 && range.total > settings.max_upload_bytes {
        return Err(FileError::TooLarge(format!(
            "this device accepts uploads up to {} bytes",
            settings.max_upload_bytes
        )));
    }
    let free = crate::files::roots::space(parent).map(|(_, free)| free);
    let floor = if floor_applies {
        settings.free_space_floor_bytes
    } else {
        0
    };
    if floor > 0
        && let Some(free) = free
        && free.saturating_sub(range.total - range.start) < floor
    {
        return Err(FileError::InsufficientStorage(format!(
            "that would leave less than {floor} bytes free on this device"
        )));
    }

    let part = part_path(&located.path, token)?;
    // On a flaky link an upload rarely *finishes*, so sweeping only after a
    // success would never run. Do it when one starts as well.
    if range.start == 0 {
        sweep_stale_parts(parent);
    }

    let have = part_len(&part);
    if have != range.start {
        // Not an error so much as an answer: here is where this device
        // actually got to, carry on from there.
        return Err(FileError::Conflict(format!(
            "this device has {have} bytes of that upload, not {}",
            range.start
        )));
    }

    Ok(ChunkPlan {
        target: located.path.clone(),
        part,
        precondition,
        granularity: located.root.granularity,
        created: existing.is_none(),
        max_bytes: settings.max_upload_bytes,
        floor,
        free,
        total: range.total,
    })
}

/// Append one chunk, refusing to write more than it said it would.
async fn append_chunk(body: Body, plan: &ChunkPlan, range: ChunkRange) -> Result<(), FileError> {
    use tokio::io::AsyncWriteExt;

    let file = tokio::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&plan.part)
        .await
        .map_err(|e| FileError::from_io(&e, "that upload"))?;
    let mut file = tokio::io::BufWriter::new(file);

    let mut written: u64 = 0;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| FileError::BadRequest(format!("the upload stopped: {e}")))?;
        written += chunk.len() as u64;
        // A chunk that overruns its own `Content-Range` would desynchronise
        // every later offset, so it is refused rather than truncated.
        if written > range.len() {
            return Err(FileError::BadRequest(
                "that chunk is longer than its Content-Range said".into(),
            ));
        }
        if plan.max_bytes > 0 && range.start + written > plan.max_bytes {
            return Err(FileError::TooLarge(format!(
                "this device accepts uploads up to {} bytes",
                plan.max_bytes
            )));
        }
        if plan.floor > 0
            && let Some(free) = plan.free
            && free.saturating_sub(written) < plan.floor
        {
            return Err(FileError::InsufficientStorage(format!(
                "that would leave less than {} bytes free on this device",
                plan.floor
            )));
        }
        file.write_all(&chunk)
            .await
            .map_err(|e| FileError::from_io(&e, "that upload"))?;
    }
    file.flush()
        .await
        .map_err(|e| FileError::from_io(&e, "that upload"))?;
    file.into_inner()
        .sync_all()
        .await
        .map_err(|e| FileError::from_io(&e, "that upload"))?;
    Ok(())
}

/// Publish a finished resumable upload.
fn finish_chunked(plan: ChunkPlan) -> Result<(bool, Validator, u64), FileError> {
    let have = part_len(&plan.part);
    if have != plan.total {
        return Err(FileError::BadRequest(format!(
            "this device holds {have} bytes, and the upload said {}",
            plan.total
        )));
    }
    // Again, and this is the half that matters: something may have appeared at
    // the target while the upload was in flight, and the caller said whether
    // that was allowed to be overwritten.
    let existing = match std::fs::symlink_metadata(&plan.target) {
        Ok(meta) => Some(meta),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(FileError::from_io(&e, "that file")),
    };
    plan.precondition
        .check(existing.as_ref(), plan.granularity)?;

    publish(
        &plan.part,
        &plan.target,
        matches!(plan.precondition, Precondition::MustNotExist),
    )
    .map_err(|e| publish_error(&e))?;
    let meta = std::fs::metadata(&plan.target).map_err(|e| FileError::from_io(&e, "that file"))?;
    Ok((
        plan.created,
        validator_of(&meta, plan.granularity),
        meta.len(),
    ))
}

/// Create a directory, parents included.
pub async fn mkdir(State(state): State<AppState>, Json(req): Json<DirRequest>) -> Response {
    let Some(files) = state.file_manager.clone() else {
        return disabled();
    };
    let result = blocking(move || {
        let root = files.root(&req.root)?;
        if !root.writable {
            return Err(FileError::Forbidden("that place is read-only".into()));
        }
        make_dirs(&root.canonical, &req.path, files.denied())
    })
    .await;
    match result {
        Ok(Ok(true)) => StatusCode::CREATED.into_response(),
        Ok(Ok(false)) => StatusCode::OK.into_response(),
        Ok(Err(e)) | Err(e) => e.into_response(),
    }
}

/// Rename or move, inside one root.
pub async fn move_entry(State(state): State<AppState>, Json(req): Json<MoveRequest>) -> Response {
    let Some(files) = state.file_manager.clone() else {
        return disabled();
    };
    let result = blocking(move || {
        let from = files.locate(&req.root, &req.from)?;
        if !from.root.writable {
            return Err(FileError::Forbidden("that place is read-only".into()));
        }
        let to = files.locate(&req.root, &req.to)?;
        if from.path == from.root.canonical || to.path == to.root.canonical {
            return Err(FileError::Forbidden(
                "the top of a place cannot be moved".into(),
            ));
        }
        std::fs::symlink_metadata(&from.path)
            .map_err(|e| FileError::from_io(&e, "what you asked to move"))?;
        match std::fs::symlink_metadata(&to.path) {
            Ok(existing) => {
                if !req.overwrite {
                    return Err(FileError::Conflict(
                        "something is already there; send overwrite = true to replace it".into(),
                    ));
                }
                if existing.is_dir() {
                    return Err(FileError::Conflict(
                        "a folder is already there, and this will not replace one".into(),
                    ));
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(FileError::from_io(&e, "where you asked to move it")),
        }
        std::fs::rename(&from.path, &to.path).map_err(|e| match e.raw_os_error() {
            // EXDEV. Cannot happen for two paths in one root today, since a
            // root is one filesystem — but a bind mount inside a home would
            // make it possible, and the honest answer is not a 500.
            Some(18) => FileError::BadRequest(
                "those two places are on different disks; download and re-upload instead".into(),
            ),
            // EINVAL, which `rename(2)` uses for exactly one thing a person
            // can do by accident: dragging a folder into a folder inside
            // itself. A client should refuse the gesture before it gets here,
            // and this is what stops the one that does not from reading as a
            // fault in the device.
            Some(22) => FileError::BadRequest("a folder cannot be moved inside itself".into()),
            // EISDIR and ENOTDIR: the target changed kind between the check
            // above and this call. Racy rather than wrong, so it is a conflict
            // and not an internal error.
            Some(21) => FileError::Conflict("a folder is already there".into()),
            Some(20) => FileError::Conflict("that path is not a folder".into()),
            // ENOTEMPTY, if the target became a non-empty directory in the
            // same window.
            Some(39) => {
                FileError::Conflict("a folder is already there, and it is not empty".into())
            }
            _ => FileError::from_io(&e, "that move"),
        })
    })
    .await;
    match result {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(e)) | Err(e) => e.into_response(),
    }
}

/// Delete a file or a directory.
pub async fn delete_entry(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<DeleteQuery>,
) -> Response {
    let Some(files) = state.file_manager.clone() else {
        return disabled();
    };
    let precondition = match Precondition::from_headers(&headers, false) {
        Ok(p) => p,
        Err(e) => return e.into_response(),
    };
    let result = blocking(move || {
        let located = files.locate(&q.root, &q.path)?;
        if !located.root.writable {
            return Err(FileError::Forbidden("that place is read-only".into()));
        }
        // With a handle, `path` is the folder and the handle is the entry.
        // The folder still goes through the resolver, so every escape check
        // applies to it exactly as it would to any other request; the handle
        // only ever adds one validated component to a directory the caller has
        // already been granted.
        let target = match &q.handle {
            Some(handle) => {
                let name = crate::files::decode_handle(handle)?;
                let dir = located.follow(files.denied())?;
                if !dir.is_dir() {
                    return Err(FileError::BadRequest(
                        "a handle names something inside a folder; `path` must be that folder"
                            .into(),
                    ));
                }
                let target = dir.join(&name);
                crate::files::resolve::check_denied(&target, files.denied())?;
                target
            }
            None => located.path.clone(),
        };
        if target == located.root.canonical {
            return Err(FileError::Forbidden(
                "the top of a place cannot be deleted".into(),
            ));
        }
        let meta = std::fs::symlink_metadata(&target)
            .map_err(|e| FileError::from_io(&e, "what you asked to delete"))?;
        precondition.check(Some(&meta), located.root.granularity)?;

        if meta.is_dir() {
            if q.recursive {
                // Does not follow symlinks: `remove_dir_all` unlinks a link
                // rather than descending through it, so a link into `/`
                // deletes the link.
                std::fs::remove_dir_all(&target)
            } else {
                std::fs::remove_dir(&target)
            }
            .map_err(|e| match e.raw_os_error() {
                // ENOTEMPTY / EEXIST
                Some(39) | Some(17) => FileError::Conflict(
                    "that folder is not empty; send recursive = true to delete what is in it"
                        .into(),
                ),
                _ => FileError::from_io(&e, "that folder"),
            })
        } else {
            std::fs::remove_file(&target).map_err(|e| FileError::from_io(&e, "that file"))
        }
    })
    .await;
    match result {
        Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(e)) | Err(e) => e.into_response(),
    }
}

// ---------------------------------------------------------------------------
// Preconditions
// ---------------------------------------------------------------------------

/// What the caller believes is at the path they are about to change.
///
/// Required on `PUT` and on `DELETE` alike. The config route's reasoning
/// carries over — "a device's policy has three writers … `If-Match` is
/// required rather than optional so that forgetting it is a 428 rather than a
/// clobber" — and a file has more writers than a policy does: this API, the
/// activities running at the same uid, and whoever is at the device.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Precondition {
    /// `If-None-Match: *` — must not exist.
    MustNotExist,
    /// `If-Match: *` — whatever is there.
    Any,
    /// `If-Match: "<etag>"` — must exist, and be this version.
    Exactly(String),
}

impl Precondition {
    fn from_headers(headers: &HeaderMap, allow_create: bool) -> Result<Self, FileError> {
        if let Some(inm) = header_str(headers, header::IF_NONE_MATCH) {
            if !allow_create {
                return Err(FileError::BadRequest(
                    "If-None-Match means nothing here; send If-Match".into(),
                ));
            }
            if inm.trim() != "*" {
                return Err(FileError::BadRequest(
                    "If-None-Match is only understood as '*' here".into(),
                ));
            }
            return Ok(Self::MustNotExist);
        }
        match header_str(headers, header::IF_MATCH) {
            Some(im) if im.trim() == "*" => Ok(Self::Any),
            Some(im) => Ok(Self::Exactly(unquote(im).to_string())),
            None if allow_create => Err(FileError::PreconditionRequired(
                "send If-None-Match: * to create, If-Match with the version you read to \
                 replace, or If-Match: * to replace whatever is there"
                    .into(),
            )),
            None => Err(FileError::PreconditionRequired(
                "send If-Match with the version you read, or If-Match: * to delete whatever \
                 is there"
                    .into(),
            )),
        }
    }

    /// Test against what is actually on disk.
    ///
    /// `granularity` is the filesystem's, because a tag it cannot promise to
    /// change with the content must not satisfy an `If-Match` — the caller
    /// would be replacing or deleting something other than what it read.
    fn check(
        &self,
        existing: Option<&std::fs::Metadata>,
        granularity: Granularity,
    ) -> Result<(), FileError> {
        match (self, existing) {
            (Self::MustNotExist, None) => Ok(()),
            (Self::MustNotExist, Some(_)) => Err(FileError::PreconditionFailed(
                "something is already there".into(),
            )),
            (Self::Any, _) => Ok(()),
            (Self::Exactly(_), None) => Err(FileError::PreconditionFailed(
                "nothing is there any more".into(),
            )),
            (Self::Exactly(_), Some(meta)) if meta.is_dir() => Err(FileError::PreconditionFailed(
                "a folder has no version to match; send If-Match: *".into(),
            )),
            (Self::Exactly(want), Some(meta)) => {
                let validator = validator_of(meta, granularity);
                if validator.strongly_matches(want) {
                    Ok(())
                } else if validator.weak
                    && want.trim().trim_start_matches("W/").trim_matches('"') == validator.value
                {
                    // The tag the caller holds still *looks* right, and on any
                    // other filesystem it would be. Saying "it changed" here
                    // would be a lie about a file nobody touched, so say the
                    // true thing instead — and it is actionable, because the
                    // tick passes on its own.
                    Err(FileError::PreconditionFailed(
                        "this drive records times too coarsely to be sure that is still \
                         the same file; try again in a moment"
                            .into(),
                    ))
                } else {
                    Err(FileError::PreconditionFailed(
                        "that has changed since you read it; read it again and redo the change"
                            .into(),
                    ))
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// Open a file for download, having proved it is the file that was checked.
///
/// The `dev`/`ino` comparison closes the gap between deciding a path is
/// allowed and opening it: a symlink swapped in between the two would
/// otherwise hand out whatever it now points at. Opening the *canonical* path
/// removes the links; comparing the open file's identity to the one that was
/// validated removes the race.
fn open_for_read(
    located: &Located,
    denied: &[PathBuf],
) -> Result<(std::fs::File, std::fs::Metadata, String), FileError> {
    let canonical = located.follow(denied)?;

    let checked = std::fs::metadata(&canonical).map_err(|e| FileError::from_io(&e, "that file"))?;
    if checked.is_dir() {
        return Err(FileError::BadRequest(
            "that is a folder; list it instead".into(),
        ));
    }
    if !checked.is_file() {
        return Err(FileError::BadRequest(
            "that is not a file this device will hand over".into(),
        ));
    }

    let file = std::fs::File::open(&canonical).map_err(|e| FileError::from_io(&e, "that file"))?;
    let opened = file
        .metadata()
        .map_err(|e| FileError::from_io(&e, "that file"))?;
    if (opened.dev(), opened.ino()) != (checked.dev(), checked.ino()) {
        return Err(FileError::Internal(
            "that file was replaced while it was being opened".into(),
        ));
    }

    let name = located
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "download".to_string());
    Ok((file, opened, name))
}

/// Headers every download carries. See the module comment: all three of these
/// are why an uploaded `.html` cannot run as this origin.
fn download_headers(
    name: &str,
    validator: &crate::files::Validator,
    content_range: Option<String>,
) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    // Every one of these is ASCII by construction -- the filename is
    // percent-encoded and the tag is `<digits>-<digits>` -- so a failure here
    // is unreachable. Skipping rather than unwrapping so that an unreachable
    // case cannot become a panic in a request handler.
    let disposition = format!("attachment; filename*=UTF-8''{}", percent_encode(name));
    if let Ok(value) = HeaderValue::try_from(disposition) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    if let Ok(value) = HeaderValue::try_from(validator.header()) {
        headers.insert(header::ETAG, value);
    }
    if let Some(range) = content_range
        && let Ok(value) = HeaderValue::try_from(range)
    {
        headers.insert(header::CONTENT_RANGE, value);
    }
    headers
}

/// `bytes=a-b`, `bytes=a-`, `bytes=-n`. One range only.
///
/// A multi-range request gets the whole file with `200` instead, which the
/// specification allows and which nothing we ship ever sends.
fn parse_range(value: &str, len: u64) -> Result<(u64, u64), String> {
    let spec = value
        .trim()
        .strip_prefix("bytes=")
        .ok_or_else(|| "only byte ranges are understood".to_string())?;
    if spec.contains(',') {
        return Err("only one range at a time".into());
    }
    let (start, end) = spec
        .split_once('-')
        .ok_or_else(|| "that range is not a range".to_string())?;
    let (start, end) = match (start.trim(), end.trim()) {
        ("", "") => return Err("that range is empty".into()),
        // Suffix: the last N bytes.
        ("", n) => {
            let n: u64 = n.parse().map_err(|_| "that range is not a number")?;
            if n == 0 {
                return Err("a zero-length suffix range asks for nothing".into());
            }
            (len.saturating_sub(n), len.saturating_sub(1))
        }
        (s, "") => {
            let s: u64 = s.parse().map_err(|_| "that range is not a number")?;
            (s, len.saturating_sub(1))
        }
        (s, e) => {
            let s: u64 = s.parse().map_err(|_| "that range is not a number")?;
            let e: u64 = e.parse().map_err(|_| "that range is not a number")?;
            (s, e.min(len.saturating_sub(1)))
        }
    };
    if len == 0 || start >= len || start > end {
        return Err(format!("this file is {len} bytes long"));
    }
    Ok((start, end))
}

// ---------------------------------------------------------------------------
// Writing
// ---------------------------------------------------------------------------

/// Everything the streaming half needs, decided before the body is touched.
struct WritePlan {
    target: PathBuf,
    temp: PathBuf,
    /// The destination filesystem's, for the tag in the answer.
    granularity: Granularity,
    /// Whether the target existed, for `201` versus `200`.
    created: bool,
    /// Whether the caller said `If-None-Match: *`, which the rename has to
    /// keep rather than merely to have checked. See [`publish`].
    must_not_exist: bool,
    /// 0 means no cap.
    max_bytes: u64,
    /// Bytes that must remain free on the destination filesystem afterwards.
    floor: u64,
    /// Free space as it was before the write started.
    free: Option<u64>,
}

fn prepare_write(
    located: &Located,
    precondition: Precondition,
    declared: Option<u64>,
    settings: &shepherd_config::FileManagerConfig,
    floor_applies: bool,
) -> Result<WritePlan, FileError> {
    if !located.root.writable {
        return Err(FileError::Forbidden("that place is read-only".into()));
    }
    if located.path == located.root.canonical {
        return Err(FileError::BadRequest("name a file to write".into()));
    }
    let Some(parent) = located.path.parent() else {
        return Err(FileError::BadRequest("name a file to write".into()));
    };
    let parent_meta =
        std::fs::metadata(parent).map_err(|e| FileError::from_io(&e, "that folder"))?;
    if !parent_meta.is_dir() {
        return Err(FileError::Conflict("that is a file, not a folder".into()));
    }

    let existing = match std::fs::symlink_metadata(&located.path) {
        Ok(meta) => Some(meta),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(FileError::from_io(&e, "that file")),
    };
    if let Some(meta) = &existing {
        if meta.is_dir() {
            return Err(FileError::Conflict("that name belongs to a folder".into()));
        }
        if meta.file_type().is_symlink() {
            // Replacing *through* a link is how an upload ends up somewhere
            // nobody chose. The rename below would replace the link itself,
            // which is defensible — but silently turning a link into a file is
            // not what anyone asked for either.
            return Err(FileError::Conflict(
                "that name is a link; delete it first".into(),
            ));
        }
    }
    precondition.check(existing.as_ref(), located.root.granularity)?;

    if settings.max_upload_bytes > 0
        && let Some(len) = declared
        && len > settings.max_upload_bytes
    {
        return Err(FileError::TooLarge(format!(
            "this device accepts uploads up to {} bytes",
            settings.max_upload_bytes
        )));
    }

    let free = crate::files::roots::space(parent).map(|(_, free)| free);
    let floor = if floor_applies {
        settings.free_space_floor_bytes
    } else {
        0
    };
    if floor > 0
        && let (Some(free), Some(len)) = (free, declared)
        && free.saturating_sub(len) < floor
    {
        return Err(FileError::InsufficientStorage(format!(
            "that would leave less than {floor} bytes free on this device"
        )));
    }

    // Dotted, and in the destination directory: dotted so a listing hides it
    // by default and the sweep below can recognise it, and in the destination
    // directory so the rename that publishes it is atomic rather than a copy
    // across filesystems.
    let name = located
        .path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "upload".into());
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let temp = parent.join(format!(".{name}.{}-{stamp}.part", std::process::id()));

    Ok(WritePlan {
        target: located.path.clone(),
        temp,
        granularity: located.root.granularity,
        created: existing.is_none(),
        must_not_exist: matches!(precondition, Precondition::MustNotExist),
        max_bytes: settings.max_upload_bytes,
        floor,
        free,
    })
}

/// Stream the body into the plan's temp file, enforcing the caps as it grows.
///
/// The declared `Content-Length` was checked before this; it can also be a
/// lie, which is why both limits are checked again per chunk.
async fn stream_to_temp(body: Body, plan: &WritePlan) -> Result<u64, FileError> {
    use tokio::io::AsyncWriteExt;

    // `create_new`: the temp name must not exist, so nothing can be tricked
    // into writing through a link somebody left lying around.
    let file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&plan.temp)
        .await
        // Named for what the caller asked for, not for the mechanism: the temp
        // file is this API's business, and a person who typed a name that the
        // drive will not take should be told about *that* name.
        .map_err(|e| FileError::from_io(&e, "that file"))?;
    let mut file = tokio::io::BufWriter::new(file);

    let mut written: u64 = 0;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| FileError::BadRequest(format!("the upload stopped: {e}")))?;
        written += chunk.len() as u64;
        if plan.max_bytes > 0 && written > plan.max_bytes {
            return Err(FileError::TooLarge(format!(
                "this device accepts uploads up to {} bytes",
                plan.max_bytes
            )));
        }
        if plan.floor > 0
            && let Some(free) = plan.free
            && free.saturating_sub(written) < plan.floor
        {
            return Err(FileError::InsufficientStorage(format!(
                "that would leave less than {} bytes free on this device",
                plan.floor
            )));
        }
        file.write_all(&chunk)
            .await
            .map_err(|e| FileError::from_io(&e, "that upload"))?;
    }
    file.flush()
        .await
        .map_err(|e| FileError::from_io(&e, "that upload"))?;
    // Through to the platter before the rename publishes it: a file that
    // appears complete and is not is worse than one that never appeared.
    file.into_inner()
        .sync_all()
        .await
        .map_err(|e| FileError::from_io(&e, "that upload"))?;
    Ok(written)
}

/// Publish the temp file, and tidy up after uploads that never finished.
/// Put a finished upload in place of the target.
///
/// A plain `rename(2)` replaces whatever is there, which is right for every
/// precondition but one. `If-None-Match: *` is a promise that nothing is
/// overwritten, and `prepare_write` can only check that a moment *before* the
/// bytes arrive — long enough for a second administrator, or for this caller's
/// own retry, to create the same name in between. Two uploads then both answer
/// `201` and one person's file is silently gone.
///
/// `RENAME_NOREPLACE` moves the promise into the kernel, where it is a single
/// atomic operation. Filesystems that do not implement it — vfat among them,
/// which matters here because removable drives are a first-class root — answer
/// `EINVAL` or `ENOSYS`, and there is nothing better to do on those than the
/// plain rename the code did before. The window stays open on a USB stick and
/// is closed everywhere else, which is the right way round: the device's own
/// disk is where two administrators are actually both writing.
fn publish(from: &Path, to: &Path, must_not_exist: bool) -> std::io::Result<()> {
    if must_not_exist {
        use nix::errno::Errno;
        use nix::fcntl::{RenameFlags, renameat2};
        match renameat2(None, from, None, to, RenameFlags::RENAME_NOREPLACE) {
            Ok(()) => return Ok(()),
            // Somebody got there first, between the check and now.
            Err(Errno::EEXIST) => {
                return Err(std::io::Error::from(std::io::ErrorKind::AlreadyExists));
            }
            // The filesystem has no such call. Fall through.
            Err(Errno::EINVAL | Errno::ENOSYS | Errno::EOPNOTSUPP | Errno::EPERM) => {}
            Err(e) => return Err(std::io::Error::from_raw_os_error(e as i32)),
        }
    }
    std::fs::rename(from, to)
}

/// `EEXIST` out of [`publish`] is the caller's precondition failing late, not a
/// name collision the caller could not have known about, so it reads as the
/// same `412` a collision found early does.
fn publish_error(e: &std::io::Error) -> FileError {
    if e.kind() == std::io::ErrorKind::AlreadyExists {
        FileError::PreconditionFailed(
            "something else created that name while this upload was in flight".into(),
        )
    } else {
        FileError::from_io(e, "that file")
    }
}

fn finish_write(plan: WritePlan, size: u64) -> Result<(bool, Validator, u64), FileError> {
    publish(&plan.temp, &plan.target, plan.must_not_exist).map_err(|e| {
        let _ = std::fs::remove_file(&plan.temp);
        publish_error(&e)
    })?;
    let meta = std::fs::metadata(&plan.target).map_err(|e| FileError::from_io(&e, "that file"))?;
    if let Some(parent) = plan.target.parent() {
        sweep_stale_parts(parent);
    }
    Ok((plan.created, validator_of(&meta, plan.granularity), size))
}

/// Remove `.part` files in `dir` older than a day.
///
/// **One directory, and opportunistic.** Not recursive, and not on a timer:
/// it runs where an upload is landing and where somebody is looking, which
/// between them cover every folder anybody has reason to care about. A
/// daemon-wide daily walk was the alternative and is not worth it — every root
/// includes directories with tens of thousands of files in them, and a scan of
/// those is the child's evening rather than a housekeeping task.
///
/// The consequence, stated so nobody has to rediscover it: a transfer
/// abandoned into a folder that is never uploaded to or opened again keeps its
/// part file. It is dotted, so it is reported as a hidden entry rather than
/// concealed, and a person who turns hidden files on can delete it themselves.
///
/// An upload whose connection dropped leaves one behind — the handler removes
/// what it knows about, but a daemon that was killed mid-upload knows nothing.
/// A day is long enough that no live upload is ever caught by it.
fn sweep_stale_parts(dir: &Path) {
    const STALE: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with('.') && name.ends_with(".part")) {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .map(|m| m.elapsed().map(|age| age > STALE).unwrap_or(false))
            .unwrap_or(false);
        if stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// `mkdir -p`, one component at a time so every one of them is checked.
///
/// [`crate::files::resolve::resolve`] cannot do this: it canonicalises the
/// parent, and the parent of a directory being created may not exist yet.
/// Walking the components keeps the guarantee — each new level is created
/// inside a directory that has just been proved to be inside the root.
///
/// Returns whether anything was created.
fn make_dirs(root: &Path, rel: &str, denied: &[PathBuf]) -> Result<bool, FileError> {
    let parts = crate::files::resolve::components(rel)?;
    if parts.is_empty() {
        return Err(FileError::BadRequest("name a folder to create".into()));
    }
    let mut current = root.to_path_buf();
    let mut created = false;
    for part in parts {
        let candidate = current.join(part);
        match std::fs::symlink_metadata(&candidate) {
            Ok(meta) if meta.is_dir() => {}
            Ok(meta) if meta.file_type().is_symlink() => {
                // Only if it still lands inside; an escaping link is refused
                // here exactly as it is everywhere else.
                if !crate::files::resolve::link_stays_inside(&candidate, root, denied) {
                    return Err(FileError::Forbidden(
                        "that path leads outside the folder it was asked for".into(),
                    ));
                }
            }
            Ok(_) => {
                // Deliberately not naming the component. The caller sent the
                // path and already knows what is in it, so echoing one of its
                // pieces back buys nothing — and an error message is a poor
                // place to reflect bytes somebody else chose, whatever the
                // content type says.
                return Err(FileError::Conflict(
                    "that path runs through a file, so a folder cannot go there".into(),
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&candidate)
                    .map_err(|e| FileError::from_io(&e, "that folder"))?;
                created = true;
            }
            Err(e) => return Err(FileError::from_io(&e, "that folder")),
        }
        current =
            std::fs::canonicalize(&candidate).map_err(|e| FileError::from_io(&e, "that folder"))?;
        if !current.starts_with(root) {
            return Err(FileError::Forbidden(
                "that path leads outside the folder it was asked for".into(),
            ));
        }
        crate::files::resolve::check_denied(&current, denied)?;
    }
    Ok(created)
}

// ---------------------------------------------------------------------------
// Small shared pieces
// ---------------------------------------------------------------------------

/// Run blocking filesystem work off the runtime's worker threads.
///
/// Every route does this. `tokio::fs` would spawn per call; one hop per
/// request keeps a listing of ten thousand entries from becoming ten thousand
/// of them.
async fn blocking<T, F>(f: F) -> Result<T, FileError>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| FileError::Internal(format!("that file operation panicked: {e}")))
}

/// The router only mounts these routes when the file manager is configured on,
/// so this is unreachable on a device — it exists so a misassembled router is
/// a 404 rather than a panic.
fn disabled() -> Response {
    FileError::NotFound("this device does not offer remote file management".into()).into_response()
}

fn header_str(headers: &HeaderMap, name: header::HeaderName) -> Option<&str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

fn unquote(value: &str) -> &str {
    value.trim().trim_matches('"')
}

/// RFC 5987 percent-encoding for a `filename*` parameter.
///
/// Everything outside the unreserved set, so a book called `Mr O'Malley & Son`
/// downloads under its own name instead of breaking the header.
fn percent_encode(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for byte in name.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_parse() {
        assert_eq!(parse_range("bytes=0-9", 100), Ok((0, 9)));
        assert_eq!(parse_range("bytes=10-", 100), Ok((10, 99)));
        assert_eq!(parse_range("bytes=-10", 100), Ok((90, 99)));
        // Past the end is clamped, as the specification asks.
        assert_eq!(parse_range("bytes=90-200", 100), Ok((90, 99)));
        assert!(parse_range("bytes=100-", 100).is_err());
        assert!(parse_range("bytes=0-9,20-29", 100).is_err());
        assert!(parse_range("items=0-9", 100).is_err());
        assert!(parse_range("bytes=0-0", 0).is_err());
    }

    #[test]
    fn filenames_survive_the_header() {
        assert_eq!(percent_encode("the-hobbit.epub"), "the-hobbit.epub");
        assert_eq!(
            percent_encode("Mr O'Malley & Son"),
            "Mr%20O%27Malley%20%26%20Son"
        );
        assert_eq!(percent_encode("café.txt"), "caf%C3%A9.txt");
    }
}
