//! What the file routes do when the filesystem is not an ordinary one
//! (issue #195).
//!
//! `tests/files.rs` covers the wire contract: statuses, preconditions,
//! headers, escapes. This covers the things underneath it that a protocol test
//! cannot see — a name that is not text, a file that is not a file, two writers
//! at once, a reader while a writer is renaming, and a body too big to hold in
//! memory. Each of these is something a kiosk's home directory really contains
//! after a year: an activity's socket, a half-copied file from a camera, a
//! filename written by a program that does not care about UTF-8.
//!
//! The concurrency tests assert *invariants that hold in every interleaving*
//! rather than one expected outcome, so they are deterministic even though
//! what happens is not.

use std::os::unix::ffi::OsStrExt;
use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::Value;
use shepherd_config::FileManagerConfig;
use shepherd_http::{AppState, FileService, handlers};
use tower::ServiceExt;

mod support;
use support::make_service;

fn fixture() -> (tempfile::TempDir, Router) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("Books")).unwrap();
    let state = AppState {
        svc: make_service(),
        file_manager: Some(Arc::new(FileService::fixed(
            dir.path().to_path_buf(),
            FileManagerConfig {
                // The host running the tests may well have a drive mounted
                // under /media, and a test that listed it would pass or fail
                // by accident.
                external_media: false,
                ..Default::default()
            },
        ))),
    };
    let app = handlers::router(
        state,
        shepherd_http::AuthSources::without_credential_store(),
    );
    (dir, app)
}

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, Value, header::HeaderMap) {
    let response = app.clone().oneshot(req).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json, headers)
}

async fn body_of(app: &Router, req: Request<Body>) -> (StatusCode, Vec<u8>, header::HeaderMap) {
    let response = app.clone().oneshot(req).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, bytes.to_vec(), headers)
}

fn get(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

fn create(uri: &str, body: impl Into<Body>) -> Request<Body> {
    Request::builder()
        .method("PUT")
        .uri(uri)
        .header(header::IF_NONE_MATCH, "*")
        .body(body.into())
        .unwrap()
}

/// The row for `name` in a listing, or `None`.
fn row<'a>(listing: &'a Value, name: &str) -> Option<&'a Value> {
    listing["entries"]
        .as_array()?
        .iter()
        .find(|e| e["name"] == name)
}

// ---------------------------------------------------------------------------
// Names that are not text
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_name_that_is_not_utf8_is_shown_but_cannot_be_addressed() {
    let (dir, app) = fixture();
    // Byte 0xFF is not valid UTF-8 anywhere. A camera, an old archive, or a
    // program written before anyone cared will produce one of these, and the
    // listing must not simply fall over.
    let raw = std::ffi::OsStr::from_bytes(b"holiday-\xffphoto.jpg");
    std::fs::write(dir.path().join("Books").join(raw), b"jpeg").unwrap();
    std::fs::write(dir.path().join("Books/ordinary.txt"), b"text").unwrap();

    let (status, body, _) = send(&app, get("/api/v1/files/list?root=home&path=Books")).await;
    assert_eq!(status, StatusCode::OK);

    // Lossily, because JSON has no way to say what the name really is — and
    // labelled, because the replacement character is not something the caller
    // can send back.
    let lossy = String::from_utf8_lossy(b"holiday-\xffphoto.jpg").into_owned();
    let entry = row(&body, &lossy).expect("the un-nameable file was dropped from the listing");
    assert_eq!(entry["unusable"], "name_not_utf8");
    // The ordinary file beside it is unaffected: one bad name does not cost a
    // person the rest of the folder.
    assert!(row(&body, "ordinary.txt").unwrap()["unusable"].is_null());

    // And the lossy spelling addresses nothing, which is the honest outcome:
    // it names a file that does not exist.
    let (status, _, _) = send(
        &app,
        get("/api/v1/files/content?root=home&path=Books/holiday-%EF%BF%BDphoto.jpg"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// Files that are not files
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_fifo_is_marked_special_and_never_opened() {
    let (dir, app) = fixture();
    let fifo = dir.path().join("Books/pipe");
    nix::unistd::mkfifo(
        &fifo,
        nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
    )
    .unwrap();

    let (status, body, _) = send(&app, get("/api/v1/files/list?root=home&path=Books")).await;
    assert_eq!(status, StatusCode::OK);
    let entry = row(&body, "pipe").expect("the fifo was not listed");
    assert_eq!(entry["kind"], "other");
    assert_eq!(entry["unusable"], "special_file");
    assert!(entry["size"].is_null());

    // The important half. `open(2)` on a fifo with no writer *blocks forever*,
    // so a download route that opened first and asked questions afterwards
    // would hang a worker thread until the daemon was restarted. The check is
    // a `stat` before the open, and this test is what keeps it in that order:
    // it does not merely assert a 400, it asserts one arrives at all.
    let opened = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        send(&app, get("/api/v1/files/content?root=home&path=Books/pipe")),
    )
    .await
    .expect("the download route blocked on a fifo");
    assert_eq!(opened.0, StatusCode::BAD_REQUEST);
    assert_eq!(opened.1["error"], "bad_request");
}

#[tokio::test]
async fn a_fifo_can_still_be_deleted() {
    let (dir, app) = fixture();
    let fifo = dir.path().join("Books/pipe");
    nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::S_IRUSR).unwrap();

    // Being unable to *read* it is not a reason to be unable to get rid of it:
    // a stale socket left by a crashed activity is exactly the thing a person
    // opens this page to clean up. There is no etag for a fifo, so the
    // precondition is the `*` form.
    let (status, _, _) = send(
        &app,
        Request::builder()
            .method("DELETE")
            .uri("/api/v1/files/entry?root=home&path=Books/pipe")
            .header(header::IF_MATCH, "*")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(!fifo.exists());
}

// ---------------------------------------------------------------------------
// Recursive delete
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_recursive_delete_unlinks_a_link_rather_than_walking_through_it() {
    let (dir, app) = fixture();
    // Somewhere outside the root, with something in it worth keeping.
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("payroll.csv"), b"do not delete me").unwrap();

    std::fs::create_dir_all(dir.path().join("Books/album")).unwrap();
    std::fs::write(dir.path().join("Books/album/cover.png"), b"png").unwrap();
    // Any activity running at the kiosk uid can plant this, so it is not a
    // hypothetical: a `remove_dir_all` that followed it would delete a
    // person's documents from inside a folder they thought they were tidying.
    std::os::unix::fs::symlink(outside.path(), dir.path().join("Books/album/elsewhere")).unwrap();

    let (status, _, _) = send(
        &app,
        Request::builder()
            .method("DELETE")
            .uri("/api/v1/files/entry?root=home&path=Books/album&recursive=true")
            .header(header::IF_MATCH, "*")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    assert!(
        !dir.path().join("Books/album").exists(),
        "the folder survived"
    );
    assert!(
        outside.path().join("payroll.csv").exists(),
        "a recursive delete followed a symlink out of the root"
    );
    assert!(outside.path().exists());
}

// ---------------------------------------------------------------------------
// The `.part` sweep
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_sweep_takes_a_days_old_part_and_nothing_else() {
    let (dir, app) = fixture();
    let books = dir.path().join("Books");

    let stale = books.join(".film.bin.999-1.part");
    let fresh = books.join(".other.bin.999-2.part");
    let decoy = books.join(".config");
    let plain = books.join("film.part");
    for path in [&stale, &fresh, &decoy, &plain] {
        std::fs::write(path, b"x").unwrap();
    }
    // A day and an hour ago: an upload nobody is coming back to finish.
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(25 * 60 * 60);
    for path in [&stale, &decoy, &plain] {
        filetime::set_file_mtime(path, filetime::FileTime::from_system_time(old)).unwrap();
    }

    // Any completed upload into the directory runs the sweep.
    let (status, _, _) = send(
        &app,
        create("/api/v1/files/content?root=home&path=Books/new.epub", "hi"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    assert!(!stale.exists(), "a day-old part file was left behind");
    // A resumable upload in progress is not rubbish, however slow the link is.
    assert!(fresh.exists(), "an upload still in flight was swept away");
    // And the sweep is not a licence to delete other people's dotfiles, or
    // anything a person named themselves.
    assert!(
        decoy.exists(),
        "the sweep took a dotfile that was not a part"
    );
    assert!(plain.exists(), "the sweep took a file a person had named");
}

// ---------------------------------------------------------------------------
// Two writers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn two_creators_of_the_same_name_leave_one_whole_file() {
    let (dir, app) = fixture();
    let first = "a".repeat(64 * 1024);
    let second = "b".repeat(64 * 1024);

    let (one, two) = tokio::join!(
        send(
            &app,
            create(
                "/api/v1/files/content?root=home&path=Books/race.txt",
                first.clone()
            ),
        ),
        send(
            &app,
            create(
                "/api/v1/files/content?root=home&path=Books/race.txt",
                second.clone()
            ),
        ),
    );

    // Exactly one creates. The other is told the name was taken — never a
    // second `201`, and never a silent merge. `prepare_write` checked that
    // the name was free *before* 64 KiB of body arrived, which is long enough
    // for the other writer to take it; the promise is kept at the rename, by
    // `RENAME_NOREPLACE`.
    let statuses = [one.0, two.0];
    assert!(
        statuses.contains(&StatusCode::CREATED),
        "neither writer created the file: {statuses:?}"
    );
    assert!(
        statuses.contains(&StatusCode::PRECONDITION_FAILED)
            || statuses.contains(&StatusCode::CONFLICT),
        "both writers thought they created the file: {statuses:?}"
    );

    // And what is on disk is one of the two bodies, whole. Each upload
    // streams to its own dotted temp file and is renamed in, so there is no
    // interleaving to see.
    let written = std::fs::read_to_string(dir.path().join("Books/race.txt")).unwrap();
    assert!(
        written == first || written == second,
        "the file is {} bytes and matches neither writer",
        written.len()
    );
}

#[tokio::test]
async fn two_chunks_at_the_same_offset_never_publish_a_spliced_file() {
    let (dir, app) = fixture();
    let uri = "/api/v1/files/content?root=home&path=Books/film.bin&upload=racetoken01";
    let half = vec![b'A'; 32 * 1024];
    let rest = vec![b'B'; 32 * 1024];
    let total = half.len() + rest.len();

    let chunk = |range: String, bytes: Vec<u8>| {
        Request::builder()
            .method("PUT")
            .uri(uri)
            .header(header::IF_NONE_MATCH, "*")
            .header(header::CONTENT_RANGE, range)
            .body(Body::from(bytes))
            .unwrap()
    };

    // Two clients — or one client whose retry arrived after the original —
    // both believing the device is at offset zero.
    let (one, two) = tokio::join!(
        send(
            &app,
            chunk(format!("bytes 0-{}/{total}", half.len() - 1), half.clone())
        ),
        send(
            &app,
            chunk(format!("bytes 0-{}/{total}", half.len() - 1), half.clone())
        ),
    );
    // At most one may be accepted; the loser is told where the device really
    // is, which is the protocol's re-sync and not an error the caller has to
    // interpret.
    let accepted = [one.0, two.0]
        .iter()
        .filter(|s| **s == StatusCode::NO_CONTENT)
        .count();
    assert!(
        accepted >= 1,
        "both chunks were refused: {:?}",
        [one.0, two.0]
    );
    for (status, body) in [(one.0, &one.1), (two.0, &two.1)] {
        assert!(
            status == StatusCode::NO_CONTENT || status == StatusCode::CONFLICT,
            "a racing chunk answered {status}: {body}"
        );
    }

    // Finish from wherever the device actually got to, as a real client would
    // after the 409.
    let offset = {
        let (status, body, _) = send(
            &app,
            get("/api/v1/files/upload?root=home&path=Books/film.bin&upload=racetoken01"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        body["offset"].as_u64().unwrap() as usize
    };

    let finished = send(
        &app,
        chunk(
            format!("bytes {offset}-{}/{total}", total - 1),
            rest.clone(),
        ),
    )
    .await;

    // Whatever happened above, the *published* file is either absent or
    // exactly what was sent. A file that is 96 KiB — two copies of the first
    // chunk plus the second — must never appear under its real name.
    let path = dir.path().join("Books/film.bin");
    if path.exists() {
        let written = std::fs::read(&path).unwrap();
        let mut want = half.clone();
        want.extend_from_slice(&rest);
        assert_eq!(
            written,
            want,
            "a spliced file was published ({} bytes, status {})",
            written.len(),
            finished.0
        );
    }
}

#[tokio::test]
async fn an_upload_racing_a_delete_never_leaves_the_old_bytes_behind() {
    let (dir, app) = fixture();
    let path = dir.path().join("Books/hobbit.epub");
    std::fs::write(&path, b"the first edition").unwrap();

    let (upload, deleted) = tokio::join!(
        send(
            &app,
            Request::builder()
                .method("PUT")
                .uri("/api/v1/files/content?root=home&path=Books/hobbit.epub")
                .header(header::IF_MATCH, "*")
                .body(Body::from("the second edition"))
                .unwrap(),
        ),
        send(
            &app,
            Request::builder()
                .method("DELETE")
                .uri("/api/v1/files/entry?root=home&path=Books/hobbit.epub")
                .header(header::IF_MATCH, "*")
                .body(Body::empty())
                .unwrap(),
        ),
    );
    assert!(upload.0.is_success(), "the upload answered {}", upload.0);
    assert!(
        deleted.0 == StatusCode::NO_CONTENT || deleted.0 == StatusCode::NOT_FOUND,
        "the delete answered {}",
        deleted.0
    );

    // Two orderings are possible and both are fine. What must not happen is
    // the first edition surviving: whichever ran last, it either replaced
    // those bytes or removed them.
    match std::fs::read(&path) {
        Ok(bytes) => assert_eq!(bytes, b"the second edition"),
        Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::NotFound),
    }
}

// ---------------------------------------------------------------------------
// A reader while a writer is renaming
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_reader_is_never_handed_half_of_each_version() {
    let (dir, app) = fixture();
    let path = dir.path().join("Books/film.bin");
    let first = vec![b'A'; 512 * 1024];
    let second = vec![b'B'; 512 * 1024];
    std::fs::write(&path, &first).unwrap();

    // Replaces the file the way this API does — write a temp file beside it
    // and rename — over and over, while the route is reading it.
    let writing = path.clone();
    let (a, b) = (first.clone(), second.clone());
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = stop.clone();
    let writer = std::thread::spawn(move || {
        let temp = writing.with_extension("tmp");
        let mut which = false;
        while !flag.load(std::sync::atomic::Ordering::Relaxed) {
            std::fs::write(&temp, if which { &a } else { &b }).unwrap();
            std::fs::rename(&temp, &writing).unwrap();
            which = !which;
        }
    });

    for _ in 0..40 {
        let (status, bytes, _) = body_of(
            &app,
            get("/api/v1/files/content?root=home&path=Books/film.bin"),
        )
        .await;
        // The rename is atomic and the open file handle keeps the old inode
        // alive, so a reader that started before the swap finishes reading the
        // version it started on. A 500 is the `dev`/`ino` check catching the
        // one window where that is not true; what may never happen is a body
        // that is part of each.
        if status == StatusCode::INTERNAL_SERVER_ERROR {
            continue;
        }
        assert_eq!(status, StatusCode::OK);
        assert!(
            bytes == first || bytes == second,
            "a reader got {} bytes belonging to neither version",
            bytes.len()
        );
    }

    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    writer.join().unwrap();
}

// ---------------------------------------------------------------------------
// Something too big to hold
// ---------------------------------------------------------------------------

/// Big enough that nothing on either side can be quietly buffering it whole
/// without showing up, small enough that CI does not notice. The real files
/// this device carries — a film, a ROM set — are an order of magnitude bigger
/// again, which is why the route streams rather than reads.
const BIG: usize = 24 * 1024 * 1024;

#[tokio::test]
async fn a_file_larger_than_any_buffer_goes_up_and_comes_back_whole() {
    let (dir, app) = fixture();
    // Not all one byte: a repeating 251-byte cycle (251 is prime, so it lines
    // up with no power-of-two buffer) means an off-by-a-chunk error shows as a
    // mismatch rather than as more of the same.
    let payload: Vec<u8> = (0..BIG).map(|i| (i % 251) as u8).collect();

    let (status, body, _) = send(
        &app,
        create(
            "/api/v1/files/content?root=home&path=Books/film.bin",
            payload.clone(),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["size"], BIG);
    assert_eq!(
        std::fs::metadata(dir.path().join("Books/film.bin"))
            .unwrap()
            .len(),
        BIG as u64
    );

    let (status, bytes, headers) = body_of(
        &app,
        get("/api/v1/files/content?root=home&path=Books/film.bin"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bytes.len(), BIG);
    assert!(bytes == payload, "the bytes came back changed");
    let etag = headers[header::ETAG].to_str().unwrap().to_string();

    // And a range out of the middle, which is the shape a resumed download
    // takes: the seek has to land in the right place in a file this size, and
    // an `i32` or a 16 MiB assumption anywhere would show here.
    let start = BIG - 3 * 1024 * 1024;
    let (status, bytes, headers) = body_of(
        &app,
        Request::builder()
            .uri("/api/v1/files/content?root=home&path=Books/film.bin")
            .header(header::RANGE, format!("bytes={start}-"))
            .header(header::IF_RANGE, &etag)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        headers[header::CONTENT_RANGE].to_str().unwrap(),
        format!("bytes {start}-{}/{BIG}", BIG - 1)
    );
    assert_eq!(bytes, payload[start..]);
}

#[tokio::test]
async fn a_large_file_can_be_sent_in_chunks_and_arrives_identical() {
    let (dir, app) = fixture();
    let payload: Vec<u8> = (0..BIG).map(|i| (i % 251) as u8).collect();
    // Deliberately not a divisor of the total, so the last chunk is short —
    // the case an off-by-one in the range arithmetic gets wrong.
    let chunk_bytes = 5 * 1024 * 1024;

    let mut offset = 0usize;
    while offset < BIG {
        let end = (offset + chunk_bytes).min(BIG);
        let req = Request::builder()
            .method("PUT")
            .uri("/api/v1/files/content?root=home&path=Books/film.bin&upload=bigupload01")
            .header(header::IF_NONE_MATCH, "*")
            .header(
                header::CONTENT_RANGE,
                format!("bytes {offset}-{}/{BIG}", end - 1),
            )
            .body(Body::from(payload[offset..end].to_vec()))
            .unwrap();
        let (status, body, headers) = send(&app, req).await;
        if end == BIG {
            assert_eq!(status, StatusCode::CREATED, "{body}");
            assert_eq!(body["size"], BIG);
        } else {
            assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
            assert_eq!(headers["upload-offset"].to_str().unwrap(), end.to_string());
        }
        offset = end;
    }

    assert_eq!(
        std::fs::read(dir.path().join("Books/film.bin")).unwrap(),
        payload
    );
    // The part file goes with the rename; nothing dotted is left behind for
    // the sweep to find a day later.
    let leftovers: Vec<_> = std::fs::read_dir(dir.path().join("Books"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".part"))
        .collect();
    assert!(leftovers.is_empty(), "left behind: {leftovers:?}");
}
