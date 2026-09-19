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

fn post(uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
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

/// The other half of the sweep, and the reason it exists.
///
/// The upload call sites only ever reach the directory an upload is landing
/// in, so a transfer abandoned into a folder nobody uploads to again would
/// keep its bytes for good. Opening the folder is the other moment anybody has
/// a reason to care — and it is the moment somebody is actually looking at it.
#[tokio::test]
async fn opening_a_folder_collects_what_was_abandoned_in_it() {
    let (dir, app) = fixture();
    let books = dir.path().join("Books");
    std::fs::create_dir_all(books.join("2019")).unwrap();

    // A film somebody gave up on, in a folder they never uploaded to again.
    let abandoned = books.join("2019/.film.bin.u-abc12345.part");
    std::fs::write(&abandoned, vec![b'x'; 4096]).unwrap();
    let fresh = books.join("2019/.other.bin.u-def67890.part");
    std::fs::write(&fresh, b"still going").unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(25 * 60 * 60);
    filetime::set_file_mtime(&abandoned, filetime::FileTime::from_system_time(old)).unwrap();

    // Listing a *different* folder does not reach it: the sweep is one
    // directory deep, deliberately.
    let (status, _, _) = send(&app, get("/api/v1/files/list?root=home&path=Books")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(abandoned.exists(), "the sweep descended into a subfolder");

    let (status, body, _) = send(&app, get("/api/v1/files/list?root=home&path=Books/2019")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !abandoned.exists(),
        "a day-old part survived its folder being opened"
    );
    // Swept before the listing was built, so it is not reported as being
    // somewhere it no longer is.
    assert!(
        row(&body, ".film.bin.u-abc12345.part").is_none(),
        "the listing named a file it had just deleted"
    );

    // An upload still in flight is not rubbish, however slow the link is — and
    // it is *listed*, as a hidden entry, so a person can see and remove it
    // themselves rather than wondering where their space went.
    assert!(fresh.exists(), "an upload in progress was swept away");
    let row = row(&body, ".other.bin.u-def67890.part").expect("the live part was hidden entirely");
    assert_eq!(row["hidden"], true);
}

#[tokio::test]
async fn a_read_only_place_is_listed_without_trying_to_tidy_it() {
    let dir = tempfile::tempdir().unwrap();
    let outside = dir.path().join("library");
    std::fs::create_dir_all(&outside).unwrap();
    let stale = outside.join(".film.bin.u-abc12345.part");
    std::fs::write(&stale, b"x").unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(25 * 60 * 60);
    filetime::set_file_mtime(&stale, filetime::FileTime::from_system_time(old)).unwrap();

    let mut perms = std::fs::metadata(&outside).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o555);
    std::fs::set_permissions(&outside, perms).unwrap();

    let state = AppState {
        svc: make_service(),
        file_manager: Some(Arc::new(FileService::fixed(
            dir.path().to_path_buf(),
            FileManagerConfig {
                external_media: false,
                extra_roots: vec![shepherd_config::FileManagerRoot {
                    label: "Library".into(),
                    path: outside.clone(),
                }],
                ..Default::default()
            },
        ))),
    };
    let app = handlers::router(
        state,
        shepherd_http::AuthSources::without_credential_store(),
    );

    let (status, body, _) = send(&app, get("/api/v1/files/list?root=extra-0&path=")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["writable"], false);
    // Nothing to delete here and no permission to do it with, so the sweep is
    // not attempted at all — the file stays, and is reported honestly.
    assert!(stale.exists());
    assert!(row(&body, ".film.bin.u-abc12345.part").is_some());

    // Leave it removable by the tempdir's own cleanup.
    let mut perms = std::fs::metadata(&outside).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&outside, perms).unwrap();
}

/// An entry the directory names and then refuses to explain.
///
/// A directory with read but not *search* permission is the deterministic way
/// to arrange it: `readdir` hands back every name, and `fstatat` on each of
/// them is `EACCES`. The same shape happens for real when a FAT drive's
/// charset cannot spell a stored name — the rendering readdir returns is not
/// something the filesystem can look up again — and when another writer
/// unlinks an entry between the read and the row being built.
///
/// This used to `continue`, and the file vanished from the listing with
/// nothing said. A listing that is quietly short is the one answer a file
/// manager must never give.
#[tokio::test]
async fn an_entry_that_cannot_be_explained_is_still_listed() {
    if nix::unistd::geteuid().is_root() {
        eprintln!(
            "[SKIP] an_entry_that_cannot_be_explained_is_still_listed: running as root, \
             which bypasses the directory permission this test is built on."
        );
        return;
    }
    let (dir, app) = fixture();
    let locked = dir.path().join("Books/locked");
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::write(locked.join("film.bin"), b"x").unwrap();
    std::fs::write(locked.join("poster.png"), b"y").unwrap();

    // Readable, not searchable.
    let mut perms = std::fs::metadata(&locked).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o444);
    std::fs::set_permissions(&locked, perms).unwrap();

    let (status, body, _) = send(&app, get("/api/v1/files/list?root=home&path=Books/locked")).await;
    assert_eq!(status, StatusCode::OK);

    let entries = body["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2, "the listing was quietly short: {body}");
    for name in ["film.bin", "poster.png"] {
        let entry = row(&body, name).unwrap_or_else(|| panic!("{name} was dropped: {body}"));
        // Named, and honest about knowing nothing else.
        assert_eq!(entry["unusable"], "unreadable");
        assert!(entry["size"].is_null());
        assert!(entry["modified"].is_null());
        assert!(entry["etag"].is_null());
        // Not "special_file", which would claim it is a socket or a device —
        // something was asserted about it that nobody had learned.
        assert_ne!(entry["unusable"], "special_file");
    }

    // Put it back so the tempdir can clean up after itself.
    let mut perms = std::fs::metadata(&locked).unwrap().permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&locked, perms).unwrap();
}

/// The ordinary case still says nothing, so the new field is not noise.
#[tokio::test]
async fn a_folder_that_reads_cleanly_reports_nothing_unreadable() {
    let (_dir, app) = fixture();
    let (status, body, _) = send(&app, get("/api/v1/files/list?root=home&path=Books")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.get("unreadable").is_none(),
        "a clean listing grew a field: {body}"
    );
}

// ---------------------------------------------------------------------------
// Naming a file whose name cannot be typed
// ---------------------------------------------------------------------------

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[tokio::test]
async fn a_file_whose_name_is_not_text_can_still_be_got_rid_of() {
    let (dir, app) = fixture();
    // What a FAT stick mounted with a charset that cannot spell the stored
    // name hands back: `café.mp3` written on Windows, read as iso8859-1.
    let raw = b"caf\xe9.mp3";
    let name = std::ffi::OsStr::from_bytes(raw);
    std::fs::write(dir.path().join("Books").join(name), b"a song").unwrap();
    // A neighbour, so "the right one went" is asserted rather than "something
    // went".
    std::fs::write(dir.path().join("Books/hobbit.epub"), b"a book").unwrap();

    let (status, body, _) = send(&app, get("/api/v1/files/list?root=home&path=Books")).await;
    assert_eq!(status, StatusCode::OK);
    let entry = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["unusable"] == "name_not_utf8")
        .expect("the un-typable file was not listed");
    // The listing is what hands the handle over; a client never builds one.
    let handle = entry["handle"]
        .as_str()
        .expect("no handle on an un-typable name");
    assert_eq!(handle, hex(raw));
    let etag = entry["etag"].as_str().expect("no etag").to_string();

    // The name it is *shown* under still addresses nothing, which is the
    // whole reason the handle exists.
    let (status, _, _) = send(
        &app,
        delete_with_handle(
            "/api/v1/files/entry?root=home&path=Books/caf%EF%BF%BD.mp3",
            "*",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // And the precondition is not weakened by going in this way.
    let (status, _, _) = send(
        &app,
        delete_with_handle(
            &format!("/api/v1/files/entry?root=home&path=Books&handle={handle}"),
            "\"0-0\"",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED);
    assert!(dir.path().join("Books").join(name).exists());

    let (status, _, _) = send(
        &app,
        delete_with_handle(
            &format!("/api/v1/files/entry?root=home&path=Books&handle={handle}"),
            &etag,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(!dir.path().join("Books").join(name).exists());
    // And nothing beside it went with it.
    assert!(dir.path().join("Books/hobbit.epub").exists());
}

/// A handle is a **name**, not a path, and the difference is the whole of its
/// safety: the folder it applies to goes through the resolver like any other
/// request, and the handle may only add one component to it.
#[tokio::test]
async fn a_forged_handle_is_not_a_way_around_the_resolver() {
    let (dir, app) = fixture();
    std::fs::write(dir.path().join("secret.txt"), b"not yours").unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("payroll.csv"), b"keep").unwrap();

    let forged = [
        ("a traversal", hex(b"../secret.txt")),
        ("a separator", hex(b"sub/deeper")),
        (
            "an absolute path",
            hex(outside
                .path()
                .join("payroll.csv")
                .to_str()
                .unwrap()
                .as_bytes()),
        ),
        ("the parent itself", hex(b"..")),
        ("this folder", hex(b".")),
        ("a NUL", hex(b"a\0b")),
        ("nothing at all", String::new()),
        ("half a byte", "abc".to_string()),
        ("not hex", "zzzz".to_string()),
        ("too long a name", hex(&vec![b'a'; 256])),
    ];
    for (what, handle) in forged {
        let (status, body, _) = send(
            &app,
            delete_with_handle(
                &format!("/api/v1/files/entry?root=home&path=Books&handle={handle}"),
                "*",
            ),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{what} answered {status}: {body}"
        );
    }
    assert!(
        dir.path().join("secret.txt").exists(),
        "a forged handle escaped"
    );
    assert!(outside.path().join("payroll.csv").exists());

    // And the folder half is still resolved: a handle does not excuse the path
    // it is applied to.
    let (status, _, _) = send(
        &app,
        delete_with_handle(
            &format!(
                "/api/v1/files/entry?root=home&path=../escaped&handle={}",
                hex(b"x")
            ),
            "*",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn an_ordinary_name_carries_no_handle() {
    let (_dir, app) = fixture();
    let (status, body, _) = send(&app, get("/api/v1/files/list?root=home&path=Books")).await;
    assert_eq!(status, StatusCode::OK);
    // A folder of five thousand ROMs should not grow a field per row for the
    // sake of the one case in a thousand folders that needs it.
    for entry in body["entries"].as_array().unwrap() {
        assert!(entry.get("handle").is_none(), "{entry} carries a handle");
    }
}

/// The `???.zip` collision, which a handle deliberately does **not** solve.
#[tokio::test]
async fn a_handle_cannot_separate_names_the_filesystem_itself_conflates() {
    // Three files whose rendered names are byte-identical would produce three
    // identical handles, because a handle *is* the rendered bytes. Recorded as
    // a test so the limit is not mistaken for an oversight: the fix for that
    // case is mounting the drive with a charset that can spell its contents,
    // and no API can invent a distinction the filesystem will not make.
    let (dir, app) = fixture();
    let raw = b"caf\xe9.mp3";
    std::fs::write(
        dir.path()
            .join("Books")
            .join(std::ffi::OsStr::from_bytes(raw)),
        b"x",
    )
    .unwrap();
    let (_, body, _) = send(&app, get("/api/v1/files/list?root=home&path=Books")).await;
    let handle = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["unusable"] == "name_not_utf8")
        .unwrap()["handle"]
        .as_str()
        .unwrap()
        .to_string();
    // It is a function of the bytes and nothing else — no inode, no ordering.
    assert_eq!(handle, hex(raw));
}

/// The repair, rather than the bin.
///
/// Deleting an un-typable file was the first thing offered and is the lesser
/// one: what a person actually wants is to keep the file and give it a name
/// they can use. That is a rename, and a rename could not name its own source.
#[tokio::test]
async fn a_file_whose_name_is_not_text_can_be_given_one_that_is() {
    let (dir, app) = fixture();
    let raw = b"caf\xe9.mp3";
    let name = std::ffi::OsStr::from_bytes(raw);
    std::fs::write(dir.path().join("Books").join(name), b"a song").unwrap();

    let (_, body, _) = send(&app, get("/api/v1/files/list?root=home&path=Books")).await;
    let handle = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["unusable"] == "name_not_utf8")
        .expect("not listed")["handle"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, body, _) = send(
        &app,
        post(
            "/api/v1/files/move",
            serde_json::json!({
                "root": "home",
                // The folder, not the entry: the handle supplies the rest.
                "from": "Books",
                "to": "Books/cafe.mp3",
                "from_handle": handle,
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    // The bytes are the same file, under a name anybody can type.
    assert_eq!(
        std::fs::read_to_string(dir.path().join("Books/cafe.mp3")).unwrap(),
        "a song"
    );
    assert!(!dir.path().join("Books").join(name).exists());

    // And it is an ordinary row now — no flag, no handle.
    let (_, body, _) = send(&app, get("/api/v1/files/list?root=home&path=Books")).await;
    let entry = row(&body, "cafe.mp3").expect("the renamed file is not listed");
    assert!(entry["unusable"].is_null());
    assert!(entry.get("handle").is_none());
}

/// The same handle, used to move rather than to rename.
///
/// One route serves both, so this is really asserting that a destination in
/// another folder is not a special case — but it is the case a person reaches
/// by dragging, and it is worth having the byte-level outcome written down:
/// the file keeps its contents and loses its name, because a destination can
/// only ever be something typable.
#[tokio::test]
async fn moving_an_un_typable_name_keeps_the_bytes_and_not_the_name() {
    let (dir, app) = fixture();
    std::fs::create_dir_all(dir.path().join("Books/Albums")).unwrap();
    let raw = b"caf\xe9.mp3";
    let name = std::ffi::OsStr::from_bytes(raw);
    std::fs::write(dir.path().join("Books").join(name), b"a song").unwrap();

    let (_, body, _) = send(&app, get("/api/v1/files/list?root=home&path=Books")).await;
    let handle = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["unusable"] == "name_not_utf8")
        .expect("not listed")["handle"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, body, _) = send(
        &app,
        post(
            "/api/v1/files/move",
            serde_json::json!({
                "root": "home",
                "from": "Books",
                // The lossy rendering, which is the only thing a client can
                // type for it. U+FFFD, three bytes, and nothing like the é the
                // drive was holding.
                "to": "Books/Albums/caf\u{fffd}.mp3",
                "from_handle": handle,
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    assert_eq!(
        std::fs::read_to_string(dir.path().join("Books/Albums/caf\u{fffd}.mp3")).unwrap(),
        "a song",
        "the contents did not survive the move"
    );
    assert!(!dir.path().join("Books").join(name).exists());
    // And it is addressable now, which is the consolation for the name.
    let (_, body, _) = send(&app, get("/api/v1/files/list?root=home&path=Books/Albums")).await;
    let moved = body["entries"].as_array().unwrap();
    assert_eq!(moved.len(), 1);
    assert!(moved[0]["unusable"].is_null(), "still flagged: {body}");
    assert!(moved[0].get("handle").is_none());
}

#[tokio::test]
async fn a_forged_handle_is_no_better_on_the_move_route() {
    let (dir, app) = fixture();
    std::fs::write(dir.path().join("secret.txt"), b"not yours").unwrap();

    // The same table the delete route is held to — one helper resolves both,
    // so this is checking that both actually go through it.
    for handle in [
        hex(b"../secret.txt"),
        hex(b"sub/deeper"),
        hex(b".."),
        hex(b"a\0b"),
        String::new(),
        "zzzz".to_string(),
    ] {
        let (status, body, _) = send(
            &app,
            post(
                "/api/v1/files/move",
                serde_json::json!({
                    "root": "home",
                    "from": "Books",
                    "to": "Books/stolen.txt",
                    "from_handle": handle,
                }),
            ),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{handle:?} answered: {body}"
        );
    }
    assert!(
        dir.path().join("secret.txt").exists(),
        "a forged handle escaped"
    );
    assert!(!dir.path().join("Books/stolen.txt").exists());
}

/// A destination is always typed, so there is no handle for it — and that is
/// the point, not an omission.
#[tokio::test]
async fn a_move_cannot_invent_a_destination_nothing_can_reach() {
    let (_dir, app) = fixture();
    let (status, _, _) = send(
        &app,
        post(
            "/api/v1/files/move",
            serde_json::json!({
                "root": "home",
                "from": "Books",
                "to": "Books/x",
                "to_handle": hex(b"caf\xe9.mp3"),
            }),
        ),
    )
    .await;
    // Unknown fields are ignored rather than honoured: there is no way to ask
    // for a target that cannot be named, so no way to create one.
    assert_ne!(status, StatusCode::NO_CONTENT);
}

fn delete_with_handle(uri: &str, if_match: &str) -> Request<Body> {
    Request::builder()
        .method("DELETE")
        .uri(uri)
        .header(header::IF_MATCH, if_match)
        .body(Body::empty())
        .unwrap()
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
