//! The file routes, end to end through the router (issue #195).
//!
//! A temp directory stands in for the kiosk home. What these assert is the
//! wire contract — status codes, preconditions, headers — plus the escapes,
//! which are also unit-tested against `files::resolve` and are worth having at
//! this level too: the check being *called* is as important as the check being
//! right.

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

/// A router whose file manager is rooted at `home`.
fn app(home: &std::path::Path, settings: FileManagerConfig) -> Router {
    let state = AppState {
        svc: make_service(),
        file_manager: Some(Arc::new(FileService::fixed(home.to_path_buf(), settings))),
    };
    handlers::router(
        state,
        lunchbox_http::AuthSources::without_credential_store(),
    )
}

fn fixture() -> (tempfile::TempDir, Router) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("Books/covers")).unwrap();
    std::fs::write(dir.path().join("Books/hobbit.epub"), b"once upon a time").unwrap();
    std::fs::write(dir.path().join(".hidden"), b"x").unwrap();
    let app = app(
        dir.path(),
        FileManagerConfig {
            // The host running the tests may well have a drive mounted under
            // /media, and a test that listed it would pass or fail by accident.
            external_media: false,
            ..Default::default()
        },
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

// ---------------------------------------------------------------------------
// Roots and listing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn roots_offers_the_home_and_the_limits() {
    let (_dir, app) = fixture();
    let (status, body, _) = send(&app, get("/api/v1/files/roots")).await;
    assert_eq!(status, StatusCode::OK);
    let roots = body["roots"].as_array().unwrap();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0]["id"], "home");
    assert_eq!(roots[0]["kind"], "home");
    assert_eq!(roots[0]["writable"], true);
    assert!(roots[0]["free_bytes"].as_u64().unwrap() > 0);
    assert_eq!(
        body["limits"]["max_upload_bytes"],
        8 * 1024 * 1024 * 1024u64
    );
}

#[tokio::test]
async fn listing_sorts_folders_first_and_flags_hidden_files() {
    let (_dir, app) = fixture();
    let (status, body, _) = send(&app, get("/api/v1/files/list?root=home&path=")).await;
    assert_eq!(status, StatusCode::OK);
    let entries = body["entries"].as_array().unwrap();
    assert_eq!(entries[0]["name"], "Books");
    assert_eq!(entries[0]["kind"], "dir");
    // Hidden, but present: `~/.config/shepherd/movies.toml` is a file a parent
    // genuinely edits, so the flag is the client's business and not a filter.
    let hidden = entries.iter().find(|e| e["name"] == ".hidden").unwrap();
    assert_eq!(hidden["hidden"], true);
}

#[tokio::test]
async fn a_file_carries_a_size_mtime_tag() {
    let (_dir, app) = fixture();
    let (_, body, _) = send(&app, get("/api/v1/files/list?root=home&path=Books")).await;
    let book = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "hobbit.epub")
        .unwrap()
        .clone();
    assert_eq!(book["size"], 16);
    let etag = book["etag"].as_str().unwrap();
    assert!(etag.starts_with("16-"), "unexpected tag {etag}");
}

#[tokio::test]
async fn an_unknown_root_is_not_found() {
    let (_dir, app) = fixture();
    let (status, body, _) = send(&app, get("/api/v1/files/list?root=ext-nope&path=")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "not_found");
}

#[tokio::test]
async fn escapes_are_refused_through_the_router() {
    let (_dir, app) = fixture();
    for path in ["..", "../..", "Books/../../etc", "%2e%2e%2fetc"] {
        let uri = format!("/api/v1/files/list?root=home&path={path}");
        let (status, _, _) = send(&app, get(&uri)).await;
        assert!(
            status == StatusCode::BAD_REQUEST || status == StatusCode::NOT_FOUND,
            "'{path}' answered {status}"
        );
    }
}

#[tokio::test]
async fn a_symlink_out_of_the_root_is_listed_but_not_usable() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(dir.path().join("outside")).unwrap();
    std::fs::write(dir.path().join("outside/secret"), b"secret").unwrap();
    std::os::unix::fs::symlink(dir.path().join("outside"), home.join("escape")).unwrap();
    let app = app(
        &home,
        FileManagerConfig {
            external_media: false,
            ..Default::default()
        },
    );

    let (_, body, _) = send(&app, get("/api/v1/files/list?root=home&path=")).await;
    let escape = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "escape")
        .expect("the link should still be listed, so a person can delete it");
    assert_eq!(escape["symlink"], true);
    // A reason, not a bare flag: this one can still be deleted, which is why
    // it is listed at all.
    assert_eq!(escape["unusable"], "symlink_escapes");

    let (status, _, _) = send(&app, get("/api/v1/files/list?root=home&path=escape")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

// ---------------------------------------------------------------------------
// Download
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_download_is_always_an_attachment() {
    let (_dir, app) = fixture();
    let (status, body, headers) = body_of(
        &app,
        get("/api/v1/files/content?root=home&path=Books/hobbit.epub"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"once upon a time");
    // The three that stop an uploaded .html running as this origin.
    assert_eq!(headers[header::CONTENT_TYPE], "application/octet-stream");
    assert_eq!(
        headers[header::CONTENT_DISPOSITION],
        "attachment; filename*=UTF-8''hobbit.epub"
    );
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(headers[header::ACCEPT_RANGES], "bytes");
}

#[tokio::test]
async fn an_html_upload_still_comes_back_as_an_attachment() {
    let (dir, app) = fixture();
    std::fs::write(dir.path().join("evil.html"), b"<script>alert(1)</script>").unwrap();
    let (status, _, headers) =
        body_of(&app, get("/api/v1/files/content?root=home&path=evil.html")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers[header::CONTENT_TYPE], "application/octet-stream");
    assert!(
        headers[header::CONTENT_DISPOSITION]
            .to_str()
            .unwrap()
            .starts_with("attachment;")
    );
}

/// The escaping link is listed so that it can be got rid of, so getting rid of
/// it has to work.
#[tokio::test]
async fn an_escaping_symlink_can_still_be_deleted() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(dir.path().join("outside")).unwrap();
    std::fs::write(dir.path().join("outside/secret"), b"secret").unwrap();
    std::os::unix::fs::symlink(dir.path().join("outside"), home.join("escape")).unwrap();
    let app = app(
        &home,
        FileManagerConfig {
            external_media: false,
            ..Default::default()
        },
    );

    let (status, _, _) = send(
        &app,
        delete("/api/v1/files/entry?root=home&path=escape", Some("*")),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(!home.join("escape").exists());
    // The link went; what it pointed at did not.
    assert!(dir.path().join("outside/secret").exists());
}

/// A directory says whether things can be created in it; a listing says
/// whether its own contents can be renamed and deleted. Both, because they are
/// different questions and the second is the one delete and rename need.
#[tokio::test]
async fn writability_is_reported_for_folders_and_for_the_listing() {
    let (_dir, app) = fixture();
    let (status, body, _) = send(&app, get("/api/v1/files/list?root=home&path=")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["writable"], true);
    let books = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "Books")
        .unwrap();
    assert_eq!(books["writable"], true);
    // A file row carries no `writable`: deleting one is a permission on its
    // parent, and a field here would look like the answer without being it.
    let hidden = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == ".hidden")
        .unwrap();
    assert!(hidden.get("writable").is_none());
}

#[tokio::test]
async fn a_read_only_folder_says_so_before_it_is_used() {
    let (dir, app) = fixture();
    let locked = dir.path().join("locked");
    std::fs::create_dir(&locked).unwrap();
    std::fs::set_permissions(&locked, std::os::unix::fs::PermissionsExt::from_mode(0o555)).unwrap();

    let (_, body, _) = send(&app, get("/api/v1/files/list?root=home&path=")).await;
    let row = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "locked")
        .unwrap();
    assert_eq!(row["writable"], false);

    // And the listing of that folder agrees, which is what a client draws its
    // delete and rename controls from.
    let (_, body, _) = send(&app, get("/api/v1/files/list?root=home&path=locked")).await;
    assert_eq!(body["writable"], false);
}

/// shepherd's own directories are listed — they are in the home, and hiding
/// them would be a lie about what is on the disk — with the reason they cannot
/// be opened.
#[tokio::test]
async fn a_denied_folder_is_listed_with_its_reason() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".ssh")).unwrap();
    unsafe {
        std::env::remove_var("XDG_DATA_HOME");
        std::env::remove_var("XDG_CACHE_HOME");
    }
    let app = app(
        dir.path(),
        FileManagerConfig {
            external_media: false,
            ..Default::default()
        },
    );
    let (_, body, _) = send(&app, get("/api/v1/files/list?root=home&path=")).await;
    let ssh = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == ".ssh")
        .unwrap();
    assert_eq!(ssh["unusable"], "not_browsable");
    assert!(ssh.get("writable").is_none());
}

#[tokio::test]
async fn a_folder_cannot_be_moved_inside_itself() {
    let (_dir, app) = fixture();
    let body = serde_json::json!({
        "root": "home",
        "from": "Books",
        "to": "Books/covers/Books",
    });
    let (status, json, _) = send(&app, post("/api/v1/files/move", body)).await;
    // Not a 500: a client should refuse the gesture, and the one that does not
    // gets an answer that says what it did rather than a fault in the device.
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(json["error"], "bad_request");
    assert!(
        json["message"].as_str().unwrap().contains("inside itself"),
        "unhelpful message: {}",
        json["message"]
    );
}

#[tokio::test]
async fn a_range_request_gets_part_of_the_file() {
    let (_dir, app) = fixture();
    let req = Request::builder()
        .uri("/api/v1/files/content?root=home&path=Books/hobbit.epub")
        .header(header::RANGE, "bytes=5-8")
        .body(Body::empty())
        .unwrap();
    let (status, body, headers) = body_of(&app, req).await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body, b"upon");
    assert_eq!(headers[header::CONTENT_RANGE], "bytes 5-8/16");
}

/// What makes a *resumed* download safe: a browser continuing an interrupted
/// one sends the validator it started with, and a file that changed since must
/// arrive whole rather than stitched onto bytes from the previous version.
#[tokio::test]
async fn a_resumed_download_refuses_to_stitch_two_versions() {
    let (dir, app) = fixture();
    let uri = "/api/v1/files/content?root=home&path=Books/hobbit.epub";

    let (_, _, headers) = body_of(&app, get(uri)).await;
    let etag = headers[header::ETAG].to_str().unwrap().to_string();

    // The validator still matches: the range is honoured and the download
    // continues where it left off.
    let req = Request::builder()
        .uri(uri)
        .header(header::RANGE, "bytes=5-8")
        .header(header::IF_RANGE, &etag)
        .body(Body::empty())
        .unwrap();
    let (status, body, _) = body_of(&app, req).await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body, b"upon");

    // It no longer matches: the whole file, not four bytes from the middle of
    // a different one.
    std::fs::write(
        dir.path().join("Books/hobbit.epub"),
        b"a completely new edition",
    )
    .unwrap();
    let req = Request::builder()
        .uri(uri)
        .header(header::RANGE, "bytes=5-8")
        .header(header::IF_RANGE, &etag)
        .body(Body::empty())
        .unwrap();
    let (status, body, _) = body_of(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"a completely new edition");
}

/// The validator Firefox actually sends when it resumes a download.
///
/// Measured against a real Firefox rather than read off the specification,
/// which tells the resumption story with `If-Range`: Firefox sends `Range` +
/// **`If-Match`**, and before this was honoured a resume after the file
/// changed was answered with bytes from the new file at the old offset —
/// stitching a download that matched neither version and reporting success.
#[tokio::test]
async fn a_resume_whose_file_changed_is_refused_rather_than_stitched() {
    let (dir, app) = fixture();
    let uri = "/api/v1/files/content?root=home&path=Books/hobbit.epub";

    let (_, _, headers) = body_of(&app, get(uri)).await;
    let etag = headers[header::ETAG].to_str().unwrap().to_string();

    // Still the same file: the resume is served.
    let req = Request::builder()
        .uri(uri)
        .header(header::RANGE, "bytes=5-8")
        .header(header::IF_MATCH, &etag)
        .body(Body::empty())
        .unwrap();
    let (status, body, _) = body_of(&app, req).await;
    assert_eq!(status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(body, b"upon");

    // Changed underneath: refused, so the browser starts again instead of
    // finishing a file that is half one version and half another.
    std::fs::write(
        dir.path().join("Books/hobbit.epub"),
        b"a completely new edition",
    )
    .unwrap();
    let req = Request::builder()
        .uri(uri)
        .header(header::RANGE, "bytes=5-8")
        .header(header::IF_MATCH, &etag)
        .body(Body::empty())
        .unwrap();
    let (status, json, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED);
    assert_eq!(json["error"], "precondition_failed");
}

#[tokio::test]
async fn a_download_with_a_matching_if_match_star_is_served() {
    let (_dir, app) = fixture();
    // `*` means "any version of this file", which is what a caller sends when
    // it only cares that the file still exists.
    let req = Request::builder()
        .uri("/api/v1/files/content?root=home&path=Books/hobbit.epub")
        .header(header::IF_MATCH, "*")
        .body(Body::empty())
        .unwrap();
    let (status, body, _) = body_of(&app, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"once upon a time");
}

#[tokio::test]
async fn a_range_past_the_end_is_not_satisfiable() {
    let (_dir, app) = fixture();
    let req = Request::builder()
        .uri("/api/v1/files/content?root=home&path=Books/hobbit.epub")
        .header(header::RANGE, "bytes=99-")
        .body(Body::empty())
        .unwrap();
    let (status, _, headers) = body_of(&app, req).await;
    assert_eq!(status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(headers[header::CONTENT_RANGE], "bytes */16");
}

#[tokio::test]
async fn downloading_a_folder_is_refused() {
    let (_dir, app) = fixture();
    let (status, body, _) = send(&app, get("/api/v1/files/content?root=home&path=Books")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "bad_request");
}

// ---------------------------------------------------------------------------
// Upload
// ---------------------------------------------------------------------------

fn put(uri: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method("PUT")
        .uri(uri)
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn put_with(uri: &str, body: &str, header_name: header::HeaderName, value: &str) -> Request<Body> {
    Request::builder()
        .method("PUT")
        .uri(uri)
        .header(header_name, value)
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn an_upload_without_a_precondition_is_refused() {
    let (_dir, app) = fixture();
    let (status, body, _) = send(
        &app,
        put("/api/v1/files/content?root=home&path=Books/new.epub", "x"),
    )
    .await;
    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED);
    assert_eq!(body["error"], "precondition_required");
}

#[tokio::test]
async fn if_none_match_creates_and_then_refuses_to_clobber() {
    let (dir, app) = fixture();
    let (status, body, headers) = send(
        &app,
        put_with(
            "/api/v1/files/content?root=home&path=Books/new.epub",
            "chapter one",
            header::IF_NONE_MATCH,
            "*",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["size"], 11);
    assert!(headers.contains_key(header::ETAG));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("Books/new.epub")).unwrap(),
        "chapter one"
    );

    let (status, body, _) = send(
        &app,
        put_with(
            "/api/v1/files/content?root=home&path=Books/new.epub",
            "chapter two",
            header::IF_NONE_MATCH,
            "*",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED);
    assert_eq!(body["error"], "precondition_failed");
}

#[tokio::test]
async fn if_match_replaces_the_version_it_read_and_nothing_else() {
    let (dir, app) = fixture();
    let (_, body, _) = send(&app, get("/api/v1/files/list?root=home&path=Books")).await;
    let etag = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "hobbit.epub")
        .unwrap()["etag"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, _, _) = send(
        &app,
        put_with(
            "/api/v1/files/content?root=home&path=Books/hobbit.epub",
            "a new edition",
            header::IF_MATCH,
            &format!("\"{etag}\""),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("Books/hobbit.epub")).unwrap(),
        "a new edition"
    );

    // The tag the caller held is now stale, and the second write is refused
    // rather than quietly overwriting the first.
    let (status, _, _) = send(
        &app,
        put_with(
            "/api/v1/files/content?root=home&path=Books/hobbit.epub",
            "a third edition",
            header::IF_MATCH,
            &format!("\"{etag}\""),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED);
}

#[tokio::test]
async fn if_match_star_overwrites_whatever_is_there() {
    let (dir, app) = fixture();
    let (status, _, _) = send(
        &app,
        put_with(
            "/api/v1/files/content?root=home&path=Books/hobbit.epub",
            "replaced",
            header::IF_MATCH,
            "*",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("Books/hobbit.epub")).unwrap(),
        "replaced"
    );
}

#[tokio::test]
async fn an_upload_over_the_cap_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(
        dir.path(),
        FileManagerConfig {
            external_media: false,
            max_upload_bytes: 4,
            ..Default::default()
        },
    );
    let (status, body, _) = send(
        &app,
        put_with(
            "/api/v1/files/content?root=home&path=big.bin",
            "much too long",
            header::IF_NONE_MATCH,
            "*",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(body["error"], "too_large");
    assert!(!dir.path().join("big.bin").exists());
}

#[tokio::test]
async fn an_upload_that_would_fill_the_disk_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(
        dir.path(),
        FileManagerConfig {
            external_media: false,
            // Larger than any test machine's free space, so the floor always bites.
            free_space_floor_bytes: u64::MAX / 2,
            ..Default::default()
        },
    );
    let (status, body, _) = send(
        &app,
        put_with(
            "/api/v1/files/content?root=home&path=big.bin",
            "x",
            header::IF_NONE_MATCH,
            "*",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::INSUFFICIENT_STORAGE);
    assert_eq!(body["error"], "insufficient_storage");
}

#[tokio::test]
async fn an_upload_into_a_folder_that_is_not_there_is_refused() {
    let (_dir, app) = fixture();
    let (status, _, _) = send(
        &app,
        put_with(
            "/api/v1/files/content?root=home&path=Nowhere/new.epub",
            "x",
            header::IF_NONE_MATCH,
            "*",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_upload_leaves_no_part_file_behind() {
    let (dir, app) = fixture();
    send(
        &app,
        put_with(
            "/api/v1/files/content?root=home&path=Books/new.epub",
            "done",
            header::IF_NONE_MATCH,
            "*",
        ),
    )
    .await;
    let leftovers: Vec<_> = std::fs::read_dir(dir.path().join("Books"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".part"))
        .collect();
    assert!(leftovers.is_empty(), "left {leftovers:?} behind");
}

// ---------------------------------------------------------------------------
// Resumable uploads
//
// The reason this exists: a device on repurposed hardware has the wifi chip it
// came with, and a drop at 95% of a 4 GiB video should not cost 4 GiB.
// ---------------------------------------------------------------------------

fn chunk(
    uri: &str,
    body: &str,
    range: &str,
    precondition: (header::HeaderName, &str),
) -> Request<Body> {
    Request::builder()
        .method("PUT")
        .uri(uri)
        .header(header::CONTENT_RANGE, range)
        .header(precondition.0, precondition.1)
        .body(Body::from(body.to_string()))
        .unwrap()
}

const CREATE: (header::HeaderName, &str) = (header::IF_NONE_MATCH, "*");

#[tokio::test]
async fn an_upload_can_arrive_in_pieces() {
    let (dir, app) = fixture();
    let uri = "/api/v1/files/content?root=home&path=Books/serial.bin&upload=tok-12345678";

    let (status, _, headers) = send(&app, chunk(uri, "abcde", "bytes 0-4/10", CREATE)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // Where to carry on from, so a client that lost its place can be told.
    assert_eq!(headers["upload-offset"], "5");
    // Nothing is published until the last byte lands.
    assert!(!dir.path().join("Books/serial.bin").exists());

    let (status, body, _) = send(&app, chunk(uri, "fghij", "bytes 5-9/10", CREATE)).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["size"], 10);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("Books/serial.bin")).unwrap(),
        "abcdefghij"
    );
}

#[tokio::test]
async fn a_resumed_upload_is_told_where_it_got_to() {
    let (_dir, app) = fixture();
    let uri = "/api/v1/files/content?root=home&path=Books/serial.bin&upload=tok-12345678";
    let offset_uri = "/api/v1/files/upload?root=home&path=Books/serial.bin&upload=tok-12345678";

    // Nothing sent yet: the same question a fresh upload asks.
    let (status, body, _) = send(&app, get(offset_uri)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["offset"], 0);

    send(&app, chunk(uri, "abcde", "bytes 0-4/10", CREATE)).await;
    let (_, body, _) = send(&app, get(offset_uri)).await;
    assert_eq!(body["offset"], 5);
}

#[tokio::test]
async fn a_chunk_at_the_wrong_offset_is_told_the_right_one() {
    let (_dir, app) = fixture();
    let uri = "/api/v1/files/content?root=home&path=Books/serial.bin&upload=tok-12345678";
    send(&app, chunk(uri, "abcde", "bytes 0-4/10", CREATE)).await;

    // A client that lost track and re-sent from the start: refused, and told
    // what this device actually holds rather than left to guess.
    let (status, body, _) = send(&app, chunk(uri, "abcde", "bytes 0-4/10", CREATE)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(
        body["message"].as_str().unwrap().contains("5 bytes"),
        "unhelpful: {}",
        body["message"]
    );
}

#[tokio::test]
async fn an_abandoned_upload_takes_its_bytes_with_it() {
    let (dir, app) = fixture();
    let uri = "/api/v1/files/content?root=home&path=Books/serial.bin&upload=tok-12345678";
    let state = "/api/v1/files/upload?root=home&path=Books/serial.bin&upload=tok-12345678";
    send(&app, chunk(uri, "abcde", "bytes 0-4/10", CREATE)).await;

    let parts = || {
        std::fs::read_dir(dir.path().join("Books"))
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".part"))
            .count()
    };
    assert_eq!(parts(), 1, "the part file is the resumption state");

    let req = Request::builder()
        .method("DELETE")
        .uri(state)
        .body(Body::empty())
        .unwrap();
    let (status, _, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // A cancel that left gigabytes on a small disk until tomorrow is not a
    // cancel.
    assert_eq!(parts(), 0);
}

#[tokio::test]
async fn a_half_sent_upload_publishes_nothing() {
    let (dir, app) = fixture();
    let uri = "/api/v1/files/content?root=home&path=Books/serial.bin&upload=tok-12345678";
    send(&app, chunk(uri, "abcde", "bytes 0-4/10", CREATE)).await;

    // The listing shows no half-file, and the name is still free.
    let (_, body, _) = send(&app, get("/api/v1/files/list?root=home&path=Books")).await;
    let names: Vec<&str> = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert!(!names.contains(&"serial.bin"), "published early: {names:?}");
    assert!(!dir.path().join("Books/serial.bin").exists());
}

#[tokio::test]
async fn the_precondition_is_checked_again_at_the_end() {
    let (dir, app) = fixture();
    let uri = "/api/v1/files/content?root=home&path=Books/late.bin&upload=tok-12345678";
    send(&app, chunk(uri, "abcde", "bytes 0-4/10", CREATE)).await;

    // Somebody else put a file there while the upload was in flight. "Create,
    // do not replace" has to still mean that at the moment of the rename.
    std::fs::write(dir.path().join("Books/late.bin"), b"theirs").unwrap();

    let (status, _, _) = send(&app, chunk(uri, "fghij", "bytes 5-9/10", CREATE)).await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("Books/late.bin")).unwrap(),
        "theirs"
    );
}

#[tokio::test]
async fn an_upload_token_cannot_name_a_file_somewhere_else() {
    let (_dir, app) = fixture();
    // The token becomes part of a filename, so it is checked like every other
    // caller-supplied name on this API.
    for token in ["../escape", "has/slash", "short", "has.dot", "has space"] {
        let uri = format!(
            "/api/v1/files/content?root=home&path=Books/x.bin&upload={}",
            urlencode(token)
        );
        let (status, _, _) = send(&app, chunk(&uri, "abc", "bytes 0-2/3", CREATE)).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "token {token:?} was accepted"
        );
    }
}

#[tokio::test]
async fn a_chunk_longer_than_it_claimed_is_refused() {
    let (dir, app) = fixture();
    let uri = "/api/v1/files/content?root=home&path=Books/serial.bin&upload=tok-12345678";
    // Overrunning would desynchronise every later offset, so it is refused
    // rather than truncated.
    let (status, _, _) = send(&app, chunk(uri, "abcdefgh", "bytes 0-4/10", CREATE)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(!dir.path().join("Books/serial.bin").exists());
}

#[tokio::test]
async fn a_resumable_upload_over_the_cap_is_refused_at_the_first_chunk() {
    let dir = tempfile::tempdir().unwrap();
    let app = app(
        dir.path(),
        FileManagerConfig {
            external_media: false,
            max_upload_bytes: 4,
            ..Default::default()
        },
    );
    // Judged on the declared total, not on the chunk: the point is to refuse
    // before the transfer, not after it.
    let uri = "/api/v1/files/content?root=home&path=big.bin&upload=tok-12345678";
    let (status, body, _) = send(&app, chunk(uri, "ab", "bytes 0-1/100", CREATE)).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(body["error"], "too_large");
}

/// Percent-encode for a query string, so a token with a slash in it reaches
/// the handler as one rather than being routed.
fn urlencode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// mkdir, move, delete
// ---------------------------------------------------------------------------

fn post(uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

#[tokio::test]
async fn mkdir_creates_parents_and_is_idempotent() {
    let (dir, app) = fixture();
    let body = serde_json::json!({ "root": "home", "path": "Games/roms/snes" });
    let (status, _, _) = send(&app, post("/api/v1/files/dir", body.clone())).await;
    assert_eq!(status, StatusCode::CREATED);
    assert!(dir.path().join("Games/roms/snes").is_dir());

    // Again: nothing was created, and that is a 200 rather than a conflict, so
    // a retry after a dropped response is safe.
    let (status, _, _) = send(&app, post("/api/v1/files/dir", body)).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn mkdir_over_a_file_is_a_conflict() {
    let (_dir, app) = fixture();
    let body = serde_json::json!({ "root": "home", "path": "Books/hobbit.epub/deeper" });
    let (status, json, _) = send(&app, post("/api/v1/files/dir", body)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(json["error"], "conflict");
}

#[tokio::test]
async fn mkdir_cannot_escape_the_root() {
    let (_dir, app) = fixture();
    let body = serde_json::json!({ "root": "home", "path": "../escaped" });
    let (status, _, _) = send(&app, post("/api/v1/files/dir", body)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn move_renames_and_refuses_to_clobber() {
    let (dir, app) = fixture();
    let body = serde_json::json!({
        "root": "home",
        "from": "Books/hobbit.epub",
        "to": "Books/the-hobbit.epub",
    });
    let (status, _, _) = send(&app, post("/api/v1/files/move", body)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(dir.path().join("Books/the-hobbit.epub").exists());
    assert!(!dir.path().join("Books/hobbit.epub").exists());

    std::fs::write(dir.path().join("Books/other.epub"), b"other").unwrap();
    let body = serde_json::json!({
        "root": "home",
        "from": "Books/other.epub",
        "to": "Books/the-hobbit.epub",
    });
    let (status, json, _) = send(&app, post("/api/v1/files/move", body)).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(json["error"], "conflict");

    let body = serde_json::json!({
        "root": "home",
        "from": "Books/other.epub",
        "to": "Books/the-hobbit.epub",
        "overwrite": true,
    });
    let (status, _, _) = send(&app, post("/api/v1/files/move", body)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("Books/the-hobbit.epub")).unwrap(),
        "other"
    );
}

fn delete(uri: &str, if_match: Option<&str>) -> Request<Body> {
    let mut b = Request::builder().method("DELETE").uri(uri);
    if let Some(value) = if_match {
        b = b.header(header::IF_MATCH, value);
    }
    b.body(Body::empty()).unwrap()
}

#[tokio::test]
async fn delete_requires_a_precondition_too() {
    let (dir, app) = fixture();
    let uri = "/api/v1/files/entry?root=home&path=Books/hobbit.epub";
    let (status, json, _) = send(&app, delete(uri, None)).await;
    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED);
    assert_eq!(json["error"], "precondition_required");
    assert!(dir.path().join("Books/hobbit.epub").exists());

    let (status, _, _) = send(&app, delete(uri, Some("*"))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(!dir.path().join("Books/hobbit.epub").exists());
}

#[tokio::test]
async fn delete_refuses_a_stale_version() {
    let (_dir, app) = fixture();
    let uri = "/api/v1/files/entry?root=home&path=Books/hobbit.epub";
    let (status, json, _) = send(&app, delete(uri, Some("\"16-1\""))).await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED);
    assert_eq!(json["error"], "precondition_failed");
}

#[tokio::test]
async fn deleting_a_full_folder_needs_recursive() {
    let (dir, app) = fixture();
    let uri = "/api/v1/files/entry?root=home&path=Books";
    let (status, json, _) = send(&app, delete(uri, Some("*"))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(json["error"], "conflict");

    let (status, _, _) = send(
        &app,
        delete(
            "/api/v1/files/entry?root=home&path=Books&recursive=true",
            Some("*"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(!dir.path().join("Books").exists());
}

#[tokio::test]
async fn the_top_of_a_root_cannot_be_deleted() {
    let (_dir, app) = fixture();
    let (status, json, _) = send(
        &app,
        delete(
            "/api/v1/files/entry?root=home&path=&recursive=true",
            Some("*"),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(json["error"], "forbidden");
}

// ---------------------------------------------------------------------------
// Denied subtrees, and the off switch
// ---------------------------------------------------------------------------

#[tokio::test]
async fn shepherds_own_data_directory_is_not_browsable() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".local/share/lunchboxd")).unwrap();
    std::fs::write(dir.path().join(".local/share/lunchboxd/shepherd.db"), b"db").unwrap();
    std::fs::create_dir_all(dir.path().join(".local/state/lunchboxd")).unwrap();
    std::fs::write(
        dir.path().join(".local/state/lunchboxd/lunchboxd.log"),
        b"log",
    )
    .unwrap();
    // `denied_dirs` honours XDG, and the test process may have it set to
    // somewhere else entirely.
    unsafe {
        std::env::remove_var("XDG_DATA_HOME");
        std::env::remove_var("XDG_CACHE_HOME");
        std::env::set_var("HOME", dir.path());
    }
    let app = app(
        dir.path(),
        FileManagerConfig {
            external_media: false,
            ..Default::default()
        },
    );

    let (status, _, _) = send(
        &app,
        get("/api/v1/files/content?root=home&path=.local/share/lunchboxd/shepherd.db"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The log directory deliberately stays readable: pulling `lunchboxd.log`
    // off a device with no shell is one of the better things this buys.
    let (status, body, _) = body_of(
        &app,
        get("/api/v1/files/content?root=home&path=.local/state/lunchboxd/lunchboxd.log"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"log");
}

/// The routes are mounted on the guarded router, and this is what would catch
/// somebody moving that mount. Every one of them reaches a device's files, so
/// "it is inside the `require_auth` layer" is worth an assertion rather than a
/// reading of the router.
#[tokio::test]
async fn every_file_route_needs_a_credential() {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState {
        svc: make_service(),
        file_manager: Some(Arc::new(FileService::fixed(
            dir.path().to_path_buf(),
            FileManagerConfig {
                external_media: false,
                ..Default::default()
            },
        ))),
    };
    let app = handlers::router(
        state,
        lunchbox_http::AuthSources::without_credential_store()
            .with_static_token(Some("secret".into())),
    );

    for req in [
        get("/api/v1/files/roots"),
        get("/api/v1/files/list?root=home&path="),
        get("/api/v1/files/content?root=home&path=x"),
        put("/api/v1/files/content?root=home&path=x", "x"),
        post(
            "/api/v1/files/dir",
            serde_json::json!({"root":"home","path":"x"}),
        ),
        post(
            "/api/v1/files/move",
            serde_json::json!({"root":"home","from":"a","to":"b"}),
        ),
        delete("/api/v1/files/entry?root=home&path=x", Some("*")),
    ] {
        let uri = req.uri().path().to_string();
        let (status, _, _) = send(&app, req).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri} answered unguarded");
    }

    // And with the credential, the same request gets an answer.
    let req = Request::builder()
        .uri("/api/v1/files/roots")
        .header(header::AUTHORIZATION, "Bearer secret")
        .body(Body::empty())
        .unwrap();
    let (status, _, _) = send(&app, req).await;
    assert_eq!(status, StatusCode::OK);
}

/// A private key is a credential for somewhere else, which is the one thing on
/// this surface that "the caller already administers this device" does not
/// cover.
#[tokio::test]
async fn ssh_keys_are_not_browsable() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".ssh")).unwrap();
    std::fs::write(dir.path().join(".ssh/id_ed25519"), b"PRIVATE KEY").unwrap();
    let app = app(
        dir.path(),
        FileManagerConfig {
            external_media: false,
            ..Default::default()
        },
    );

    let (status, _, _) = send(&app, get("/api/v1/files/list?root=home&path=.ssh")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = send(
        &app,
        get("/api/v1/files/content?root=home&path=.ssh/id_ed25519"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // And it cannot be written into either — a key dropped in by this API is
    // as much of a problem as one read out of it.
    let (status, _, _) = send(
        &app,
        put_with(
            "/api/v1/files/content?root=home&path=.ssh/authorized_keys",
            "ssh-ed25519 AAAA",
            header::IF_NONE_MATCH,
            "*",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// `lunchboxd -d /somewhere/else`, or a `service.data_dir` in the config.
///
/// The environment-derived refusal guards `$XDG_DATA_HOME/lunchboxd`, which on
/// a device is where the database is and on a device started with `-d` is a
/// path nothing is at. The daemon says where its store really is, so that the
/// refusal is about the file rather than about a guess.
#[tokio::test]
async fn a_store_somewhere_else_is_refused_by_the_path_it_really_has() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("Library/state");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("lunchboxd.db"), b"sqlite").unwrap();

    let state = AppState {
        svc: make_service(),
        file_manager: Some(Arc::new(
            FileService::fixed(
                dir.path().to_path_buf(),
                FileManagerConfig {
                    external_media: false,
                    ..Default::default()
                },
            )
            .also_deny(store.clone()),
        )),
    };
    let app = handlers::router(
        state,
        lunchbox_http::AuthSources::without_credential_store(),
    );

    let (status, _, _) = send(&app, get("/api/v1/files/list?root=home&path=Library/state")).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = send(
        &app,
        get("/api/v1/files/content?root=home&path=Library/state/lunchboxd.db"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // And it cannot be written into either, which is the half that would
    // corrupt rather than merely leak.
    let (status, _, _) = send(
        &app,
        put_with(
            "/api/v1/files/content?root=home&path=Library/state/lunchboxd.db",
            "not a database",
            header::IF_MATCH,
            "*",
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The folder above it is still browsable — denying a subtree is not
    // denying its parent, and a person should be able to see that it is there.
    let (status, body, _) = send(&app, get("/api/v1/files/list?root=home&path=Library")).await;
    assert_eq!(status, StatusCode::OK);
    let row = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "state")
        .expect("the state folder vanished from its parent");
    assert_eq!(row["unusable"], "not_browsable");
}

#[tokio::test]
async fn a_device_with_it_switched_off_has_no_routes() {
    let state = AppState {
        svc: make_service(),
        file_manager: None,
    };
    let app = handlers::router(
        state,
        lunchbox_http::AuthSources::without_credential_store(),
    );
    let (status, _, _) = send(&app, get("/api/v1/files/roots")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// These routes carry more caller-chosen text than any other part of the API —
/// a filename in a listing, the path an upload echoes back — so the two headers
/// that decide how a browser treats a body are worth pinning here as well as in
/// `tests/api.rs`.
///
/// Two content types, deliberately, and the download is the interesting one:
/// everything that answers *about* files is `application/json`, and the one
/// route that answers *with* a file is a fixed `application/octet-stream`,
/// never a type derived from the name. It is listed here beside the others so
/// that the rule this pins is the true one — **never `text/html`, always
/// `nosniff`** — rather than "the file routes answer JSON", which is false of
/// the most important of them.
#[tokio::test]
async fn no_file_route_answers_html_and_none_may_be_sniffed() {
    let (dir, app) = fixture();
    std::fs::write(dir.path().join("Books/blocker"), b"x").unwrap();

    let cases: Vec<(&str, &str, Request<Body>)> = vec![
        (
            "a listing",
            "application/json",
            get("/api/v1/files/list?root=home&path=Books"),
        ),
        (
            "a refusal",
            "application/json",
            get("/api/v1/files/list?root=home&path=../escaped"),
        ),
        (
            "a conflict",
            "application/json",
            post(
                "/api/v1/files/dir",
                serde_json::json!({ "root": "home", "path": "Books/blocker/deeper" }),
            ),
        ),
        (
            "an upload",
            "application/json",
            put_with(
                "/api/v1/files/content?root=home&path=Books/new.epub",
                "chapter one",
                header::IF_NONE_MATCH,
                "*",
            ),
        ),
        // The file itself. Fixed, not guessed from the extension — and the
        // reason the rule above cannot be "everything is JSON".
        (
            "a download",
            "application/octet-stream",
            get("/api/v1/files/content?root=home&path=Books/hobbit.epub"),
        ),
        // A range and a not-modified take the same path and are asserted to,
        // because they build their headers through the same function and a
        // change that missed one would be silent.
        (
            "a ranged download",
            "application/octet-stream",
            Request::builder()
                .uri("/api/v1/files/content?root=home&path=Books/hobbit.epub")
                .header(header::RANGE, "bytes=0-3")
                .body(Body::empty())
                .unwrap(),
        ),
    ];

    for (name, want, req) in cases {
        let response = app.clone().oneshot(req).await.unwrap();
        let headers = response.headers().clone();
        let got = headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.split(';').next().unwrap_or(v).trim().to_string());
        assert_eq!(got.as_deref(), Some(want), "{name} answered the wrong type");
        assert_ne!(got.as_deref(), Some("text/html"), "{name} answered markup",);
        assert_eq!(
            headers
                .get(header::X_CONTENT_TYPE_OPTIONS)
                .and_then(|v| v.to_str().ok()),
            Some("nosniff"),
            "{name} would let a browser guess at its body",
        );
    }
}

/// A `304` carries no body, so it is checked on its own — it still has to say
/// what it would have been, and still has to be unsniffable.
#[tokio::test]
async fn a_not_modified_download_keeps_its_type_and_its_guard() {
    let (_dir, app) = fixture();
    let (_, _, headers) = body_of(
        &app,
        get("/api/v1/files/content?root=home&path=Books/hobbit.epub"),
    )
    .await;
    let etag = headers[header::ETAG].to_str().unwrap().to_string();

    let (status, _, headers) = body_of(
        &app,
        Request::builder()
            .uri("/api/v1/files/content?root=home&path=Books/hobbit.epub")
            .header(header::IF_NONE_MATCH, &etag)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_MODIFIED);
    assert_eq!(headers[header::CONTENT_TYPE], "application/octet-stream");
    assert_eq!(headers["x-content-type-options"], "nosniff");
}

/// A `409` from `mkdir` used to quote the path component back at the caller.
///
/// It told them nothing they had not just sent, and an error message is a poor
/// place to reflect bytes somebody else chose — `serde_json` escapes quotes and
/// control characters but not `<`, `>` or `&`, so the content type was the only
/// thing between a filename and a browser reading it as markup.
#[tokio::test]
async fn a_refusal_does_not_read_a_filename_back_to_the_caller() {
    let (dir, app) = fixture();
    // A name an activity at the kiosk uid could have written, or an
    // administrator could have uploaded.
    let payload = "<img src=x onerror=alert(1)>";
    std::fs::write(dir.path().join("Books").join(payload), b"x").unwrap();

    let (status, body, _) = send(
        &app,
        post(
            "/api/v1/files/dir",
            serde_json::json!({ "root": "home", "path": format!("Books/{payload}/deeper") }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    let message = body["message"].as_str().unwrap();
    assert!(
        !message.contains('<') && !message.contains(payload),
        "the refusal quoted the caller back at themselves: {message:?}"
    );
    // Still says what went wrong, which is the whole job of the message.
    assert!(message.contains("runs through a file"), "{message:?}");
}

/// Nothing about this feature reaches the RPC surface, and that is a decision
/// rather than an omission: `#[management_rpc]` carries every async trait
/// method to BLE, whose frames cap at 16 KiB.
#[tokio::test]
async fn none_of_this_is_on_the_rpc_surface() {
    let (_dir, app) = fixture();
    for method in ["list_files", "upload_file", "file_roots"] {
        let req = Request::builder()
            .method("POST")
            .uri("/api/v1/rpc")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::json!({ "method": method, "params": null }).to_string(),
            ))
            .unwrap();
        let (status, json, _) = send(&app, req).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{method} exists");
        assert_eq!(json["error"], "method_not_found");
    }
}
