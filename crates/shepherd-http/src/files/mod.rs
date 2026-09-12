//! Remote file management for the web interface (issue #195).
//!
//! A hardened kiosk account denies SSH and keeps its home at mode 0700, so
//! `scp`, `rsync` and every SFTP file manager are shut out of exactly the
//! directory a parent has to put a book, a ROM or a video into. The workaround
//! — copy in as the admin user and `sudo mv` — leaves root-owned files in a
//! child's home. shepherdd already runs *as* that user, so it is the one
//! process on the device for which none of that is a problem.
//!
//! ## Shape
//!
//! A caller names a **root** and a path inside it, and never an absolute path.
//! Roots are enumerated by the server ([`roots`]); a location it did not offer
//! has no spelling. Everything else goes through [`resolve::resolve`].
//!
//! ## Not on `ManagementService`
//!
//! These are HTTP routes, like `/api/v1/config` and for the same reason:
//! `#[management_rpc]` carries every async trait method to BLE, whose frames
//! cap at 16 KiB, and a file transfer there would be a method that exists and
//! cannot work. Nothing here reaches the companion, which is also the decision
//! recorded in `docs/ai/history/2026-09-11 004 remote-file-manager-api (#195).md`.

pub mod resolve;
pub mod roots;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Local};
use serde::Serialize;
use serde_json::json;
use shepherd_config::FileManagerConfig;
use tokio::sync::watch;

pub use roots::{RootInfo, RootKind};

/// What went wrong, in the vocabulary the rest of this API already speaks.
///
/// Deliberately **not** `ManagementError`. That enum maps `Conflict` onto
/// `412`, because a policy write has exactly one kind of conflict — a stale
/// `If-Match`. A file API needs `409` ("something is already there") and `412`
/// ("what is there is not what you expected") to mean different things, and
/// borrowing the enum is how those two quietly become one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileError {
    BadRequest(String),
    NotFound(String),
    Forbidden(String),
    Conflict(String),
    PreconditionRequired(String),
    PreconditionFailed(String),
    TooLarge(String),
    RangeNotSatisfiable(String),
    InsufficientStorage(String),
    Internal(String),
}

impl FileError {
    fn parts(&self) -> (StatusCode, &'static str, &str) {
        match self {
            Self::BadRequest(m) => (StatusCode::BAD_REQUEST, "bad_request", m),
            Self::NotFound(m) => (StatusCode::NOT_FOUND, "not_found", m),
            Self::Forbidden(m) => (StatusCode::FORBIDDEN, "forbidden", m),
            Self::Conflict(m) => (StatusCode::CONFLICT, "conflict", m),
            Self::PreconditionRequired(m) => (
                StatusCode::PRECONDITION_REQUIRED,
                "precondition_required",
                m,
            ),
            Self::PreconditionFailed(m) => {
                (StatusCode::PRECONDITION_FAILED, "precondition_failed", m)
            }
            Self::TooLarge(m) => (StatusCode::PAYLOAD_TOO_LARGE, "too_large", m),
            Self::RangeNotSatisfiable(m) => (
                StatusCode::RANGE_NOT_SATISFIABLE,
                "range_not_satisfiable",
                m,
            ),
            Self::InsufficientStorage(m) => {
                (StatusCode::INSUFFICIENT_STORAGE, "insufficient_storage", m)
            }
            Self::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, "internal", m),
        }
    }

    /// Turn an `io::Error` from an operation on `path` into the closest thing
    /// a caller can act on.
    pub fn from_io(e: &std::io::Error, what: &str) -> Self {
        match e.kind() {
            std::io::ErrorKind::NotFound => Self::NotFound(format!("{what} is not on this device")),
            std::io::ErrorKind::PermissionDenied => {
                Self::Forbidden(format!("this device's kiosk user may not touch {what}"))
            }
            std::io::ErrorKind::AlreadyExists => Self::Conflict(format!("{what} already exists")),
            // ENOSPC arriving mid-write, after the free-space check passed —
            // somebody else filled the disk while the upload was in flight.
            _ if e.raw_os_error() == Some(28) => {
                Self::InsufficientStorage("the disk filled up during the write".into())
            }
            _ => Self::Internal(format!("{what}: {e}")),
        }
    }
}

impl IntoResponse for FileError {
    fn into_response(self) -> Response {
        let (status, code, message) = self.parts();
        (status, Json(json!({ "error": code, "message": message }))).into_response()
    }
}

/// What kind of thing a directory entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    File,
    Dir,
    /// A socket, fifo or device node. Listed so it is not invisible, and not
    /// something this API will open.
    Other,
}

/// One row of a directory listing.
#[derive(Debug, Clone, Serialize)]
pub struct DirEntryInfo {
    pub name: String,
    pub kind: EntryKind,
    pub size: Option<u64>,
    pub modified: Option<DateTime<Local>>,
    /// `"<size>-<mtime_nanos>"`, opaque and compared only for equality.
    ///
    /// Deliberately not the content hash `PolicyDocument::version_of` uses: a
    /// policy is tens of kilobytes and worth hashing so that a restore from
    /// backup reads as unchanged, while a directory of ROMs is gigabytes and
    /// re-hashing it to draw a list is not the same trade.
    pub etag: Option<String>,
    pub hidden: bool,
    pub symlink: bool,
    /// Whether this API will act on the entry at all.
    ///
    /// `false` for a symlink whose target leaves the root, and for a name that
    /// is not valid UTF-8 — the latter can only be shown lossily, and a lossy
    /// name cannot be turned back into the bytes that address the right file.
    /// Nothing uploaded here can ever be in that state; it describes what was
    /// already on the disk.
    pub usable: bool,
}

/// A page of one directory.
#[derive(Debug, Clone, Serialize)]
pub struct Listing {
    pub root: String,
    pub path: String,
    pub entries: Vec<DirEntryInfo>,
    pub truncated: bool,
    pub cursor: Option<String>,
}

/// Default and maximum page sizes.
///
/// A directory holding a ROM set really does have tens of thousands of files
/// in it, and a listing that tried to be one response would be a browser tab
/// that stops responding.
pub const DEFAULT_LIMIT: usize = 1000;
pub const MAX_LIMIT: usize = 5000;

/// A root plus a path inside it that has passed every check.
///
/// The final component is deliberately *not* resolved — see
/// [`resolve::resolve`]. An operation that will follow it (a listing, a
/// download) asks [`Located::follow`] first; one that must not (an upload, a
/// delete) uses [`Located::path`] as it stands.
pub struct Located {
    pub root: RootInfo,
    pub path: PathBuf,
}

impl Located {
    /// The target with every link resolved, having proved the result is still
    /// inside the root.
    ///
    /// This is what stops `path=escape` — a symlink out of the home, which any
    /// activity at the kiosk uid can create — from listing or handing over
    /// whatever it points at. `resolve` cannot do it: not following the last
    /// component is what keeps a link swapped in after the check from being
    /// the thing the check looked at.
    pub fn follow(&self, denied: &[PathBuf]) -> Result<PathBuf, FileError> {
        let canonical =
            std::fs::canonicalize(&self.path).map_err(|e| FileError::from_io(&e, "that path"))?;
        if !canonical.starts_with(&self.root.canonical) {
            return Err(FileError::Forbidden(
                "that leads outside the folder it was asked for".into(),
            ));
        }
        resolve::check_denied(&canonical, denied)?;
        Ok(canonical)
    }
}

/// The file manager, as the HTTP handlers see it.
pub struct FileService {
    /// shepherdd's own home directory, canonicalised once.
    home: PathBuf,
    settings: watch::Receiver<Arc<FileManagerConfig>>,
    /// shepherd's own directories inside the home, refused for read and write
    /// alike. See [`resolve::check_denied`].
    denied: Vec<PathBuf>,
}

impl FileService {
    /// Build the service around a live settings channel.
    ///
    /// A channel rather than a value because `reload_config` and the policy
    /// watcher both change `extra_roots` and the caps without restarting the
    /// daemon, and a parent who edits the config in the web editor expects the
    /// next page load to reflect it.
    pub fn new(home: PathBuf, settings: watch::Receiver<Arc<FileManagerConfig>>) -> Self {
        let home = std::fs::canonicalize(&home).unwrap_or(home);
        let denied = denied_dirs(&home);
        Self {
            home,
            settings,
            denied,
        }
    }

    /// The same thing with settings that never change — tests, and an
    /// embedding with no reload path.
    pub fn fixed(home: PathBuf, settings: FileManagerConfig) -> Self {
        // The sender is dropped immediately and deliberately: a `watch`
        // receiver keeps serving the last value it was given after the sender
        // is gone, which is exactly "these settings, forever".
        let (tx, rx) = watch::channel(Arc::new(settings));
        drop(tx);
        Self::new(home, rx)
    }

    pub fn settings(&self) -> Arc<FileManagerConfig> {
        self.settings.borrow().clone()
    }

    pub fn denied(&self) -> &[PathBuf] {
        &self.denied
    }

    /// Everywhere this device will let a caller browse, right now.
    pub fn roots(&self) -> Vec<RootInfo> {
        roots::enumerate(&self.home, &self.settings())
    }

    /// One root by id.
    ///
    /// A drive unplugged mid-session lands here as `404`, which is the honest
    /// answer and what sends the UI back to the roots list.
    pub fn root(&self, id: &str) -> Result<RootInfo, FileError> {
        self.roots()
            .into_iter()
            .find(|r| r.id == id)
            .ok_or_else(|| FileError::NotFound("that place is not on this device".into()))
    }

    /// Resolve `root` + `path` into something openable.
    pub fn locate(&self, root: &str, path: &str) -> Result<Located, FileError> {
        let root = self.root(root)?;
        let resolved = resolve::resolve(&root.canonical, path, &self.denied)?;
        Ok(Located {
            root,
            path: resolved,
        })
    }
}

/// Directories inside a home this API refuses to serve.
///
/// Two of the three are about damage nobody would connect back to the edit
/// rather than about privilege — a caller who reaches these routes can already
/// rewrite the policy, which is strictly more power. The video cache keeps an
/// index that hand-deletion desynchronises, and the data directory holds a
/// SQLite database that a half-finished upload corrupts.
///
/// `~/.ssh` is the exception, and it is here on its own merits: a private key
/// is a credential for somewhere *else*, so handing it out is the one thing on
/// this surface that is not already implied by "this caller administers this
/// device". A kiosk home does not normally have one; a device whose home does
/// should not publish it over HTTP.
///
/// `~/.local/state/shepherdd` is deliberately *not* here. It is the log
/// directory, and pulling `shepherdd.log` off a device that has no shell is
/// one of the more useful things this feature does.
fn denied_dirs(home: &Path) -> Vec<PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".local/share"))
        .join("shepherdd");
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".cache"))
        .join("shepherd");
    [data, cache, home.join(".ssh")]
        .into_iter()
        // Canonical where it can be, because what it is compared against is:
        // a lexical path would not match a home reached through a symlink.
        .map(|p| std::fs::canonicalize(&p).unwrap_or(p))
        .collect()
}

/// The tag for a file, from what a `stat` already told us.
pub fn etag_of(meta: &std::fs::Metadata) -> String {
    let nanos = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{nanos}", meta.len())
}

fn modified_of(meta: &std::fs::Metadata) -> Option<DateTime<Local>> {
    meta.modified().ok().map(DateTime::<Local>::from)
}

/// Sort key: directories first, then by name, case-insensitively.
///
/// The order is the server's because the cursor is a position in it. A client
/// that re-sorted a paginated listing would show a page boundary that makes no
/// sense — and a 5000-entry cap means most directories arrive in one page and
/// the question never comes up.
fn sort_key(name: &str, kind: EntryKind) -> (u8, String) {
    let rank = if kind == EntryKind::Dir { 0 } else { 1 };
    (rank, name.to_lowercase())
}

fn encode_cursor(key: &(u8, String)) -> String {
    format!("{}\u{1f}{}", key.0, key.1)
}

fn decode_cursor(cursor: &str) -> Result<(u8, String), FileError> {
    let (rank, name) = cursor
        .split_once('\u{1f}')
        .ok_or_else(|| FileError::BadRequest("that listing cursor is not one of ours".into()))?;
    let rank: u8 = rank
        .parse()
        .map_err(|_| FileError::BadRequest("that listing cursor is not one of ours".into()))?;
    Ok((rank, name.to_string()))
}

/// Read one directory, sorted and paginated.
pub fn list_dir(
    root: &RootInfo,
    rel: &str,
    dir: &Path,
    denied: &[PathBuf],
    limit: usize,
    cursor: Option<&str>,
) -> Result<Listing, FileError> {
    let meta = std::fs::metadata(dir).map_err(|e| FileError::from_io(&e, "that folder"))?;
    if !meta.is_dir() {
        return Err(FileError::BadRequest("that is a file, not a folder".into()));
    }

    let after = cursor.map(decode_cursor).transpose()?;
    let entries = std::fs::read_dir(dir).map_err(|e| FileError::from_io(&e, "that folder"))?;

    let mut rows: Vec<((u8, String), DirEntryInfo)> = Vec::new();
    for entry in entries.flatten() {
        let raw = entry.file_name();
        let (name, utf8) = match raw.to_str() {
            Some(s) => (s.to_string(), true),
            None => (raw.to_string_lossy().into_owned(), false),
        };
        // `symlink_metadata`, so a link is reported as a link rather than as
        // whatever it points at.
        let Ok(link_meta) = entry.metadata() else {
            continue;
        };
        let symlink = link_meta.file_type().is_symlink();
        let path = entry.path();
        // What it *is* — for a link, what it points at, which is what a person
        // browsing wants to see.
        let target_meta = if symlink {
            std::fs::metadata(&path).ok()
        } else {
            Some(link_meta.clone())
        };
        let kind = match &target_meta {
            Some(m) if m.is_dir() => EntryKind::Dir,
            Some(m) if m.is_file() => EntryKind::File,
            Some(_) => EntryKind::Other,
            None => EntryKind::Other,
        };
        let usable = utf8
            && kind != EntryKind::Other
            && (!symlink || resolve::link_stays_inside(&path, &root.canonical, denied))
            && resolve::check_denied(&path, denied).is_ok();
        let info = DirEntryInfo {
            hidden: name.starts_with('.'),
            kind,
            size: target_meta
                .as_ref()
                .filter(|m| m.is_file())
                .map(|m| m.len()),
            modified: target_meta.as_ref().and_then(modified_of),
            etag: target_meta.as_ref().filter(|m| m.is_file()).map(etag_of),
            symlink,
            usable,
            name,
        };
        let key = sort_key(&info.name, info.kind);
        if let Some(after) = &after
            && key <= *after
        {
            continue;
        }
        rows.push((key, info));
    }

    rows.sort_by(|a, b| a.0.cmp(&b.0));
    let truncated = rows.len() > limit;
    rows.truncate(limit);
    let cursor = truncated.then(|| rows.last().map(|(key, _)| encode_cursor(key)));

    Ok(Listing {
        root: root.id.clone(),
        path: rel.to_string(),
        entries: rows.into_iter().map(|(_, info)| info).collect(),
        truncated,
        cursor: cursor.flatten(),
    })
}
