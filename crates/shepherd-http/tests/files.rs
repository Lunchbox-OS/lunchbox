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
use shepherd_config::FileManagerConfig;
use shepherd_http::{AppState, FileService, handlers};
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
        shepherd_http::AuthSources::without_credential_store(),
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
    assert_eq!(escape["usable"], false);

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
    std::fs::create_dir_all(dir.path().join(".local/share/shepherdd")).unwrap();
    std::fs::write(dir.path().join(".local/share/shepherdd/shepherd.db"), b"db").unwrap();
    std::fs::create_dir_all(dir.path().join(".local/state/shepherdd")).unwrap();
    std::fs::write(
        dir.path().join(".local/state/shepherdd/shepherdd.log"),
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
        get("/api/v1/files/content?root=home&path=.local/share/shepherdd/shepherd.db"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The log directory deliberately stays readable: pulling `shepherdd.log`
    // off a device with no shell is one of the better things this buys.
    let (status, body, _) = body_of(
        &app,
        get("/api/v1/files/content?root=home&path=.local/state/shepherdd/shepherdd.log"),
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
        shepherd_http::AuthSources::without_credential_store()
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

#[tokio::test]
async fn a_device_with_it_switched_off_has_no_routes() {
    let state = AppState {
        svc: make_service(),
        file_manager: None,
    };
    let app = handlers::router(
        state,
        shepherd_http::AuthSources::without_credential_store(),
    );
    let (status, _, _) = send(&app, get("/api/v1/files/roots")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
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
