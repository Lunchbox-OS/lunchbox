//! The file routes against a **real FAT drive** (issue #195).
//!
//! Every other test in this crate runs on a `tempfile::tempdir()` — that is,
//! on ext4 or tmpfs, which is the one filesystem this feature is least likely
//! to be pointed at. Removable drives are a first-class root here
//! (`external_media = true` is the default), and FAT keeps fewer promises than
//! the code was written against: two-second timestamps, a 4 GiB per-file
//! ceiling, a small alphabet for names, and a capacity smaller than the
//! free-space floor. Three bugs lived in that gap; see
//! `docs/ai/history/2026-09-14 003 what-a-real-fat-drive-found (#195).md`.
//!
//! It cannot run in CI: it needs something actually mounted under `/media`,
//! which needs root. When the mounts are missing each test prints a `[SKIP]`
//! line and passes, so the same `cargo test --include-ignored` works in both
//! places — the same shape as `crates/lunchbox-e2e/tests/firewall_real.rs`.
//!
//! Set a host up with:
//!   sudo ./scripts/integration-tests/setup-removable-dev.sh
//!
//! Run via the orchestrator, which checks the mounts first:
//!   ./scripts/integration-tests/test-removable.sh
//!
//! Or directly:
//!   cargo test -p lunchbox-http --test files_removable -- \
//!       --include-ignored --test-threads=1 --nocapture
//!
//! Tear down with:
//!   sudo ./scripts/integration-tests/setup-removable-dev.sh --teardown

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use http_body_util::BodyExt;
use serde_json::Value;
use lunchbox_config::FileManagerConfig;
use lunchbox_http::{AppState, FileService, handlers};
use tower::ServiceExt;

mod support;
use support::make_service;

/// Where the setup script mounts the writable image.
fn mount() -> PathBuf {
    PathBuf::from(
        std::env::var("SHEPHERD_TEST_FAT_MOUNT").unwrap_or_else(|_| "/media/shepherd-fat".into()),
    )
}

/// And the read-only one, which exists to exercise the `EROFS` path.
fn mount_ro() -> PathBuf {
    PathBuf::from(
        std::env::var("SHEPHERD_TEST_FAT_MOUNT_RO")
            .unwrap_or_else(|_| "/media/shepherd-fat-ro".into()),
    )
}

/// `Some(reason)` when this host cannot run the test.
///
/// Checks `/proc/mounts` rather than just the directory, because an *unmounted*
/// `/media/shepherd-fat` is an ordinary ext4 directory that would pass every
/// assertion below for the wrong reason — which is exactly the failure this
/// whole file exists to stop happening.
fn skip_reason(point: &Path, want_writable: bool) -> Option<String> {
    let Ok(mounts) = std::fs::read_to_string("/proc/mounts") else {
        return Some("/proc/mounts is not readable".into());
    };
    let found = mounts.lines().any(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        fields.len() > 2
            && fields[1] == point.to_string_lossy().replace(' ', "\\040")
            && (fields[2] == "vfat" || fields[2] == "exfat" || fields[2] == "msdos")
    });
    if !found {
        return Some(format!(
            "nothing FAT-formatted is mounted at {}",
            point.display()
        ));
    }
    let writable = nix::unistd::access(point, nix::unistd::AccessFlags::W_OK).is_ok();
    if want_writable && !writable {
        return Some(format!(
            "{} is mounted but this user cannot write to it (mount it uid={})",
            point.display(),
            nix::unistd::getuid()
        ));
    }
    if !want_writable && writable {
        return Some(format!("{} is supposed to be read-only", point.display()));
    }
    None
}

/// Print the skip line and return, or hand back the mount point.
macro_rules! drive {
    ($name:literal, $point:expr, $writable:expr) => {
        match skip_reason(&$point, $writable) {
            Some(reason) => {
                eprintln!(
                    "[SKIP] {}: {reason}.\n       \
                     Run sudo ./scripts/integration-tests/setup-removable-dev.sh. \
                     This is the expected outcome on CI.",
                    $name
                );
                return;
            }
            None => $point,
        }
    };
}

/// A router whose home is a tempdir and which offers whatever is under
/// `/media` — which, on a host the setup script has run on, is the image.
///
/// The floor is deliberately absurd: it is what makes
/// `the_free_space_floor_guards_the_device_and_not_the_drive` a real test
/// rather than one that depends on how full this machine's disk happens to be.
fn fixture() -> (tempfile::TempDir, Router) {
    let home = tempfile::tempdir().unwrap();
    let settings = FileManagerConfig {
        external_media: true,
        free_space_floor_bytes: 1 << 50,
        ..Default::default()
    };
    let state = AppState {
        svc: make_service(),
        file_manager: Some(Arc::new(FileService::fixed(
            home.path().to_path_buf(),
            settings,
        ))),
    };
    let app = handlers::router(
        state,
        lunchbox_http::AuthSources::without_credential_store(),
    );
    (home, app)
}

/// The id the roots list gave the drive at `point`, so every other request can
/// name it the way the API insists on — by opaque id, never by path.
async fn root_id(app: &Router, point: &Path) -> String {
    let (status, body, _) = send(app, get("/api/v1/files/roots")).await;
    assert_eq!(status, StatusCode::OK);
    let canonical = std::fs::canonicalize(point).unwrap();
    body["roots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| Path::new(r["path"].as_str().unwrap()) == canonical)
        .unwrap_or_else(|| panic!("the drive at {} was not offered as a root", point.display()))
        ["id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// A fresh directory on the drive, so reruns do not trip over each other.
fn scratch(point: &Path) -> PathBuf {
    let dir = point.join(format!("t{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

// ---------------------------------------------------------------------------
// Request helpers, the same shape as `tests/files.rs`
// ---------------------------------------------------------------------------

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

fn create(uri: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("PUT")
        .uri(uri)
        .header(header::IF_NONE_MATCH, "*")
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn etag(headers: &header::HeaderMap) -> String {
    headers
        .get(header::ETAG)
        .expect("no ETag")
        .to_str()
        .unwrap()
        .to_string()
}

// ---------------------------------------------------------------------------
// The drive is a root at all
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn the_drive_turns_up_as_a_root_keyed_on_its_uuid() {
    let point = drive!(
        "the_drive_turns_up_as_a_root_keyed_on_its_uuid",
        mount(),
        true
    );
    let (_home, app) = fixture();
    let (status, body, _) = send(&app, get("/api/v1/files/roots")).await;
    assert_eq!(status, StatusCode::OK);

    let canonical = std::fs::canonicalize(&point).unwrap();
    let root = body["roots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| Path::new(r["path"].as_str().unwrap()) == canonical)
        .expect("the mounted drive was not offered");
    assert_eq!(root["kind"], "external");
    assert_eq!(root["writable"], true);
    // From `/dev/disk/by-uuid`, so the root keeps its identity across a replug.
    // `ext-dev-…` is the fallback for a filesystem with no UUID, and `mkfs.vfat`
    // always writes one.
    let id = root["id"].as_str().unwrap();
    assert!(id.starts_with("ext-"), "id {id:?}");
    assert!(
        !id.starts_with("ext-dev-"),
        "the drive's UUID was not found"
    );
    // The whole point of the image being small: it is under the 2 GiB floor.
    let total = root["total_bytes"].as_u64().unwrap();
    assert!(
        total < 2 << 30,
        "the image is {total} bytes, too big to test the floor"
    );
}

// ---------------------------------------------------------------------------
// The etag, which FAT cannot keep
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn a_fresh_file_on_the_drive_is_only_weakly_tagged() {
    let point = drive!(
        "a_fresh_file_on_the_drive_is_only_weakly_tagged",
        mount(),
        true
    );
    let dir = scratch(&point);
    let (_home, app) = fixture();
    let id = root_id(&app, &point).await;
    let base = dir.file_name().unwrap().to_string_lossy().into_owned();

    let (status, _, headers) = send(
        &app,
        create(
            &format!("/api/v1/files/content?root={id}&path={base}/film.bin"),
            "the first cut",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    // Written a moment ago on a filesystem with two-second timestamps: another
    // write of the same length would land on the same tick and leave the tag
    // untouched, so the tag is not a promise and must not be spelt like one.
    assert!(
        etag(&headers).starts_with("W/"),
        "a fresh FAT file was tagged strongly: {}",
        etag(&headers)
    );

    let (status, _, headers) = body_of(
        &app,
        get(&format!(
            "/api/v1/files/content?root={id}&path={base}/film.bin"
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(etag(&headers).starts_with("W/"));

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
#[ignore]
async fn a_resume_inside_the_tick_is_refused_rather_than_stitched() {
    let point = drive!(
        "a_resume_inside_the_tick_is_refused_rather_than_stitched",
        mount(),
        true
    );
    let dir = scratch(&point);
    let (_home, app) = fixture();
    let id = root_id(&app, &point).await;
    let base = dir.file_name().unwrap().to_string_lossy().into_owned();
    let uri = format!("/api/v1/files/content?root={id}&path={base}/film.bin");

    let (status, _, headers) = send(&app, create(&uri, "AAAAAAAAAAAAAAAA")).await;
    assert_eq!(status, StatusCode::CREATED);
    let tag = etag(&headers);

    // Firefox's resume, inside the two-second window. The tag still *looks*
    // like a match — same size, same second — so a string comparison would
    // have served bytes from whatever is there now. The weak spelling is what
    // makes this a 412.
    let (status, body, _) = send(
        &app,
        Request::builder()
            .uri(&uri)
            .header(header::RANGE, "bytes=8-")
            .header(header::IF_MATCH, tag.trim_start_matches("W/"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::PRECONDITION_FAILED,
        "a FAT etag satisfied an If-Match inside its own tick: {body}"
    );

    // Chrome's resume. A weak validator may not assemble a range, so the
    // answer is the whole file rather than a splice.
    let (status, bytes, _) = body_of(
        &app,
        Request::builder()
            .uri(&uri)
            .header(header::RANGE, "bytes=8-")
            .header(header::IF_RANGE, tag.trim_start_matches("W/"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        bytes.len(),
        16,
        "a weak tag was allowed to assemble a range"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
#[ignore]
async fn once_the_tick_has_passed_the_drive_resumes_like_any_other() {
    let point = drive!(
        "once_the_tick_has_passed_the_drive_resumes_like_any_other",
        mount(),
        true
    );
    let dir = scratch(&point);
    let (_home, app) = fixture();
    let id = root_id(&app, &point).await;
    let base = dir.file_name().unwrap().to_string_lossy().into_owned();
    let uri = format!("/api/v1/files/content?root={id}&path={base}/film.bin");

    let (status, _, _) = send(&app, create(&uri, "AAAAAAAABBBBBBBB")).await;
    assert_eq!(status, StatusCode::CREATED);

    // Past the tick: no later write can land on the same second any more, so
    // the tag is as good as it is on ext4 — which is what keeps the weak
    // spelling from costing anything in the steady state.
    tokio::time::sleep(std::time::Duration::from_millis(2_500)).await;

    let (status, _, headers) = body_of(&app, get(&uri)).await;
    assert_eq!(status, StatusCode::OK);
    let tag = etag(&headers);
    assert!(!tag.starts_with("W/"), "still weak after the tick: {tag}");

    let (status, bytes, headers) = body_of(
        &app,
        Request::builder()
            .uri(&uri)
            .header(header::RANGE, "bytes=8-")
            .header(header::IF_MATCH, &tag)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(bytes, b"BBBBBBBB");
    assert_eq!(headers.get(header::CONTENT_RANGE).unwrap(), "bytes 8-15/16");

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// The free-space floor
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn the_free_space_floor_guards_the_device_and_not_the_drive() {
    let point = drive!(
        "the_free_space_floor_guards_the_device_and_not_the_drive",
        mount(),
        true
    );
    let dir = scratch(&point);
    let (_home, app) = fixture();
    let id = root_id(&app, &point).await;
    let base = dir.file_name().unwrap().to_string_lossy().into_owned();

    // The fixture's floor is 1 PiB, so no real disk can satisfy it. On the
    // device's own disk that is the whole point of the floor: refuse.
    let (status, body, _) = send(
        &app,
        create("/api/v1/files/content?root=home&path=book.epub", "x"),
    )
    .await;
    assert_eq!(status, StatusCode::INSUFFICIENT_STORAGE, "{body}");

    // On a drive somebody plugged in, it is not. Filling a USB stick costs
    // nobody a session, and a floor here made every stick smaller than the
    // floor — which is most of them — unwritable.
    let (status, body, _) = send(
        &app,
        create(
            &format!("/api/v1/files/content?root={id}&path={base}/book.epub"),
            "x",
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "the floor was applied to a removable drive: {body}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// What the drive refuses
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn a_name_the_drive_cannot_spell_is_the_caller_s_problem() {
    let point = drive!(
        "a_name_the_drive_cannot_spell_is_the_caller_s_problem",
        mount(),
        true
    );
    let dir = scratch(&point);
    let (_home, app) = fixture();
    let id = root_id(&app, &point).await;
    let base = dir.file_name().unwrap().to_string_lossy().into_owned();

    // Every one of these is a perfectly ordinary name on ext4 and an `EINVAL`
    // on FAT. They used to be `500 internal`, which tells a person their
    // device is broken rather than to rename the file.
    for name in ["a%3Ab.txt", "what%3F.txt", "a%2Ab.txt", "a%22b%22.txt"] {
        let (status, body, _) = send(
            &app,
            create(
                &format!("/api/v1/files/content?root={id}&path={base}/{name}"),
                "x",
            ),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{name} answered {status}: {body}"
        );
        assert_eq!(body["error"], "bad_request");
        let message = body["message"].as_str().unwrap();
        assert!(
            message.contains("this drive can store"),
            "{name} says {message:?}, which does not name the problem"
        );
    }

    // A directory is the same kernel call and the same refusal.
    let (status, body, _) = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/v1/files/dir")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::json!({ "root": id, "path": format!("{base}/Holiday:2026") })
                    .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("this drive can store"),
        "{body}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Deliberately not tested here: `ENAMETOOLONG` and `EFBIG`.
///
/// Neither is reachable through this API on this image, and pretending
/// otherwise would be a test that passes for the wrong reason.
///
/// - `ENAMETOOLONG`: `resolve` caps a path component at 255 **bytes**, and
///   vfat's limit is 255 **UTF-16 units**. Any name over vfat's limit is over
///   255 bytes too, so the API's own `400` always lands first.
/// - `EFBIG`: FAT32 stops at 4 GiB per file, and the test image is 512 MiB, so
///   `ENOSPC` arrives long before it. A 4 GiB image would make this suite cost
///   minutes.
///
/// Both errno mappings are covered in `files::tests::
/// a_drives_refusals_are_answers_rather_than_faults`.
#[allow(dead_code)]
struct WhyTwoErrnosAreNotHere;

#[tokio::test]
#[ignore]
async fn a_read_only_drive_says_so_instead_of_failing_the_write() {
    let point = drive!(
        "a_read_only_drive_says_so_instead_of_failing_the_write",
        mount_ro(),
        false
    );
    let (_home, app) = fixture();
    let (_, body, _) = send(&app, get("/api/v1/files/roots")).await;
    let canonical = std::fs::canonicalize(&point).unwrap();
    let root = body["roots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| Path::new(r["path"].as_str().unwrap()) == canonical)
        .expect("the read-only drive was not offered");
    // Offered, and honestly: the UI hides its upload and delete controls
    // rather than showing buttons that will 403.
    assert_eq!(root["writable"], false);
    let id = root["id"].as_str().unwrap().to_string();

    let (status, body, _) = send(
        &app,
        create(
            &format!("/api/v1/files/content?root={id}&path=book.epub"),
            "x",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"], "forbidden");
}

// ---------------------------------------------------------------------------
// And the ordinary thing, which had also never been done on FAT
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore]
async fn a_file_survives_the_round_trip_onto_a_drive_and_back() {
    let point = drive!(
        "a_file_survives_the_round_trip_onto_a_drive_and_back",
        mount(),
        true
    );
    let dir = scratch(&point);
    let (_home, app) = fixture();
    let id = root_id(&app, &point).await;
    let base = dir.file_name().unwrap().to_string_lossy().into_owned();

    // A name with a space and a non-ASCII character in it: both legal on FAT's
    // long-name entries, and both things the escaping in the roots list and
    // the `Content-Disposition` header have to survive.
    let name = "Bilbo's Journey — 01.epub";
    let encoded = urlencode(name);
    let uri = format!("/api/v1/files/content?root={id}&path={base}/{encoded}");
    let contents = "once upon a time in a hole in the ground";

    let (status, _, _) = send(&app, create(&uri, contents)).await;
    assert_eq!(status, StatusCode::CREATED);

    let (status, bytes, headers) = body_of(&app, get(&uri)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(String::from_utf8(bytes).unwrap(), contents);
    let disposition = headers
        .get(header::CONTENT_DISPOSITION)
        .unwrap()
        .to_str()
        .unwrap();
    assert!(disposition.contains("filename*=UTF-8''"), "{disposition}");

    // And the listing sees it under the same name the drive stored.
    let (status, body, _) = send(
        &app,
        get(&format!("/api/v1/files/list?root={id}&path={base}")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<&str> = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, [name]);

    let _ = std::fs::remove_dir_all(&dir);
}

/// Percent-encode everything a query string would otherwise read as syntax.
fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}
