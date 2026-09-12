//! Integration tests for the shepherd-http management API.
//!
//! Every operation goes through the single `POST /api/v1/rpc` endpoint
//! (see `handlers/rpc.rs`) into `shepherd_management::dispatch_json`.
//! The *behavior* of each method — business logic, param parsing,
//! result wrapping — is tested once, transport-free, in
//! `shepherd-management/tests/dispatch.rs`. What's left here is only
//! what's genuinely HTTP-specific:
//!
//! - **Bearer auth**: static token, BLE-derived admin authority, and
//!   the open-mode fallbacks.
//! - **Status-code mapping**: that each `RpcDispatchError` /
//!   `ManagementError` variant maps to the right HTTP status and
//!   `error` code string (`management_error_to_http` in
//!   `handlers/rpc.rs`). One representative method per arm is enough —
//!   the underlying errors are exercised exhaustively at the shared
//!   layer.

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use shepherd_api::{EntryKind, Event};
use shepherd_config::{
    AutoBrightnessPolicy, AvailabilityPolicy, BrightnessPolicy, Entry, LimitsPolicy, Policy,
    ServiceConfig, VolumePolicy,
};
use shepherd_core::CoreEngine;
use shepherd_host_api::{
    BrightnessCapabilities, BrightnessController, BrightnessResult, BrightnessStatus,
    HostCapabilities, MockHost, VolumeCapabilities, VolumeController, VolumeResult, VolumeStatus,
};
use shepherd_http::{AppState, handlers};
use shepherd_management::{AutoBrightnessState, DefaultManagementService, WebListenerHandle};
use shepherd_store::SqliteStore;
use shepherd_util::EntryId;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tempfile::NamedTempFile;
use tokio::sync::{Mutex, broadcast, watch};
use tower::ServiceExt;

// ---------------------------------------------------------------------------
// MockVolume
// ---------------------------------------------------------------------------

struct MockVolume {
    capabilities: VolumeCapabilities,
    status: std::sync::Mutex<VolumeStatus>,
}

impl MockVolume {
    fn new() -> Self {
        Self {
            capabilities: VolumeCapabilities {
                available: true,
                backend: Some("mock".into()),
                can_mute: true,
                max_volume: 100,
            },
            status: std::sync::Mutex::new(VolumeStatus {
                percent: 50,
                muted: false,
            }),
        }
    }
}

#[async_trait]
impl VolumeController for MockVolume {
    fn capabilities(&self) -> &VolumeCapabilities {
        &self.capabilities
    }

    async fn get_status(&self) -> VolumeResult<VolumeStatus> {
        Ok(self.status.lock().unwrap().clone())
    }

    async fn set_volume(&self, percent: u8) -> VolumeResult<()> {
        self.status.lock().unwrap().percent = percent;
        Ok(())
    }

    async fn volume_up(&self, step: u8) -> VolumeResult<()> {
        let mut s = self.status.lock().unwrap();
        s.percent = s.percent.saturating_add(step).min(100);
        Ok(())
    }

    async fn volume_down(&self, step: u8) -> VolumeResult<()> {
        let mut s = self.status.lock().unwrap();
        s.percent = s.percent.saturating_sub(step);
        Ok(())
    }

    async fn toggle_mute(&self) -> VolumeResult<()> {
        let mut s = self.status.lock().unwrap();
        s.muted = !s.muted;
        Ok(())
    }

    async fn set_mute(&self, muted: bool) -> VolumeResult<()> {
        self.status.lock().unwrap().muted = muted;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// MockBrightness
// ---------------------------------------------------------------------------

struct MockBrightness {
    capabilities: BrightnessCapabilities,
    status: std::sync::Mutex<BrightnessStatus>,
}

impl MockBrightness {
    fn new() -> Self {
        Self {
            capabilities: BrightnessCapabilities {
                available: true,
                backend: Some("mock".into()),
                device: Some("mock0".into()),
            },
            status: std::sync::Mutex::new(BrightnessStatus { percent: 50 }),
        }
    }
}

#[async_trait]
impl BrightnessController for MockBrightness {
    fn capabilities(&self) -> &BrightnessCapabilities {
        &self.capabilities
    }

    async fn get_status(&self) -> BrightnessResult<BrightnessStatus> {
        Ok(self.status.lock().unwrap().clone())
    }

    async fn set_brightness(&self, percent: u8) -> BrightnessResult<()> {
        self.status.lock().unwrap().percent = percent;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Test fixtures
// ---------------------------------------------------------------------------

fn test_policy() -> Policy {
    Policy {
        service: ServiceConfig::default(),
        groups: vec![],
        entries: vec![Entry {
            id: EntryId::new("test-game"),
            label: "Test Game".into(),
            icon_ref: None,
            kind: EntryKind::Process {
                command: "game".into(),
                args: vec![],
                env: HashMap::new(),
                cwd: None,
            },
            availability: AvailabilityPolicy {
                windows: vec![],
                always: true,
            },
            limits: LimitsPolicy {
                max_run: Some(Duration::from_secs(300)),
                daily_quota: None,
                cooldown: None,
                cooldown_min_session: Duration::ZERO,
                save_grace: shepherd_config::DEFAULT_SAVE_GRACE,
            },
            warnings: vec![],
            volume: None,
            brightness: None,
            disabled: false,
            disabled_reason: None,
            internet: Default::default(),
            firewall: None,
            browser: None,
            input_compat: vec![],
            input_compat_options: Default::default(),
            requires_input: vec![],
            tokens: None,
            group: None,
            xwayland_native_resolution: false,
            confirm_on_close: false,
            hud_orientation: None,
        }],
        default_warnings: vec![],
        default_max_run: Some(Duration::from_secs(3600)),
        volume: VolumePolicy::unrestricted(),
        brightness: BrightnessPolicy::default(),
        auto_brightness: AutoBrightnessPolicy::default(),
        hud_orientation: Default::default(),
    }
}

fn make_app_with_policy(
    policy: Policy,
    auth_token: Option<&str>,
    config_path: PathBuf,
) -> axum::Router {
    make_app_with_admin_and_policy(policy, auth_token, config_path, None)
}

fn make_app(auth_token: Option<&str>, config_path: PathBuf) -> axum::Router {
    make_app_with_policy(test_policy(), auth_token, config_path)
}

fn make_app_with_admin(
    auth_token: Option<&str>,
    config_path: PathBuf,
    admin: Option<Arc<dyn shepherd_management::AdminAuthority>>,
) -> axum::Router {
    make_app_with_admin_and_policy(test_policy(), auth_token, config_path, admin)
}

fn make_app_with_admin_and_policy(
    policy: Policy,
    auth_token: Option<&str>,
    config_path: PathBuf,
    admin: Option<Arc<dyn shepherd_management::AdminAuthority>>,
) -> axum::Router {
    make_app_full(policy, auth_token, config_path, admin, None, false)
}

/// The full constructor. `web` is the credential store (issue #156): `None`
/// gives a router with no login endpoints, which is the shape every test
/// written before that issue expects.
fn make_app_full(
    policy: Policy,
    auth_token: Option<&str>,
    config_path: PathBuf,
    admin: Option<Arc<dyn shepherd_management::AdminAuthority>>,
    web: Option<Arc<shepherd_management::WebAuth>>,
    secure_cookies: bool,
) -> axum::Router {
    let state = make_state_full(policy, config_path, web.clone());
    // `without_credential_store` is the pre-#156 shape: no login endpoints and
    // no sessions, which is what most of the tests below assert against. It is
    // spelled out rather than defaulted into, because on a device it is the
    // fail-open state -- and `shepherdd` cannot build it at all.
    let sources = match web {
        Some(web) => shepherd_http::AuthSources::new(web),
        None => shepherd_http::AuthSources::without_credential_store(),
    };
    handlers::router(
        state,
        sources
            .with_static_token(auth_token.map(str::to_owned))
            .with_admin(admin)
            .with_secure_cookies(secure_cookies),
    )
}

/// The service fixture on its own, for a test that wants an [`AppState`]
/// without a router around it.
fn make_state(policy: Policy, config_path: PathBuf) -> AppState {
    make_state_full(policy, config_path, None)
}

fn make_state_full(
    policy: Policy,
    config_path: PathBuf,
    web: Option<Arc<shepherd_management::WebAuth>>,
) -> AppState {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let host = Arc::new(MockHost::new());
    let volume = Arc::new(MockVolume::new());
    let brightness = Arc::new(MockBrightness::new());
    let engine = Arc::new(Mutex::new(CoreEngine::new(
        policy,
        store.clone(),
        HostCapabilities::minimal(),
    )));
    let (tx, _) = broadcast::channel::<Event>(64);
    let tx_for_fn = tx.clone();
    let (shutdown_tx, _shutdown_rx) = watch::channel(false);
    let svc = Arc::new(DefaultManagementService {
        engine,
        store,
        host,
        volume,
        brightness,
        light_sensor: None,
        auto_brightness: Arc::new(Mutex::new(AutoBrightnessState::new(false))),
        event_tx: tx,
        broadcast_fn: Arc::new(move |event: Event| {
            let _ = tx_for_fn.send(event);
        }),
        config_path,
        policy_files: None,
        media_refresh_tx: None,
        shutdown_tx,
        hidpi: Arc::new(shepherd_host_api::NoOpHidpiController),
        hud_layout: Arc::new(shepherd_host_api::NoOpHudLayoutController),
        display: Arc::new(shepherd_host_api::NoOpDisplayController),
        last_audio_state: Arc::new(tokio::sync::Mutex::new(None)),
        diagnostics: None,
        // Network status has no HTTP-specific behaviour — it is exercised
        // transport-free in `shepherd-management/tests/dispatch.rs`, so this
        // fixture is a host that cannot look.
        network: None,
        web_listener: WebListenerHandle::default(),
        web_auth: web,
        admins: Default::default(),
    });
    AppState {
        svc: svc as Arc<dyn shepherd_management::ManagementService>,
        // The file manager has its own fixture and its own test file
        // (`tests/files.rs`), because it needs a temp directory to be a root
        // of rather than a policy to be a service for.
        file_manager: None,
    }
}

/// Write a minimal valid config to a temp file
fn temp_config() -> NamedTempFile {
    let f = NamedTempFile::new().unwrap();
    std::fs::write(f.path(), "config_version = 1\n").unwrap();
    f
}

/// Send one request through the router and parse the response body as JSON.
async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, Value) {
    let response = app.clone().oneshot(req).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, json)
}

// ---------------------------------------------------------------------------
// RPC helpers
//
// Every test dispatches through `POST /api/v1/rpc` with body
// `{ "method": "<wire-name>", "params": <object|null> }`. Successful
// calls come back as 2xx with the trait method's return value as the
// body; failures return the mapped HTTP status plus
// `{ "error": <code>, "message": <string> }`.
// ---------------------------------------------------------------------------

fn rpc_request(method: &str, params: Value, token: Option<&str>) -> Request<Body> {
    let body = json!({ "method": method, "params": params });
    let mut b = Request::builder()
        .method("POST")
        .uri("/api/v1/rpc")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(t) = token {
        b = b.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    b.body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

async fn rpc(app: &axum::Router, method: &str, params: Value) -> (StatusCode, Value) {
    send(app, rpc_request(method, params, None)).await
}

async fn rpc_auth(
    app: &axum::Router,
    method: &str,
    params: Value,
    token: &str,
) -> (StatusCode, Value) {
    send(app, rpc_request(method, params, Some(token))).await
}

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------

#[tokio::test]
async fn auth_no_token_configured_allows_all() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, _) = rpc(&app, "health", json!({})).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn auth_missing_header_returns_401() {
    let cfg = temp_config();
    let app = make_app(Some("secret"), cfg.path().to_path_buf());
    let (status, body) = rpc(&app, "health", json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "unauthorized");
}

#[tokio::test]
async fn auth_wrong_token_returns_401() {
    let cfg = temp_config();
    let app = make_app(Some("secret"), cfg.path().to_path_buf());
    let (status, _) = rpc_auth(&app, "health", json!({}), "wrong").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn auth_correct_token_passes() {
    let cfg = temp_config();
    let app = make_app(Some("secret"), cfg.path().to_path_buf());
    let (status, _) = rpc_auth(&app, "health", json!({}), "secret").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn auth_admin_authority_token_passes() {
    use shepherd_management::AdminAuthority;
    use std::sync::Mutex;

    // A roster, not a single token: a device can have several administrators
    // and each has their own (issue #149).
    struct MockAdmin(Mutex<Vec<String>>);
    impl AdminAuthority for MockAdmin {
        fn verify_http_token(&self, presented: &str) -> bool {
            self.0.lock().unwrap().iter().any(|t| t == presented)
        }
        fn has_admin(&self) -> bool {
            !self.0.lock().unwrap().is_empty()
        }
    }

    let cfg = temp_config();
    let admin: Arc<dyn AdminAuthority> =
        Arc::new(MockAdmin(Mutex::new(vec!["admin-tok".to_string()])));
    let app = make_app_with_admin(None, cfg.path().to_path_buf(), Some(admin.clone()));

    // Admin token accepted.
    let (status, _) = rpc_auth(&app, "health", json!({}), "admin-tok").await;
    assert_eq!(status, StatusCode::OK);

    // Wrong token rejected even when admin is claimed.
    let (status, _) = rpc_auth(&app, "health", json!({}), "wrong").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Missing header rejected (admin is claimed → auth required).
    let (status, _) = rpc(&app, "health", json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// Every administrator's token opens the HTTP door, and revoking one closes
/// only that one (issue #149).
#[tokio::test]
async fn auth_every_admin_token_is_accepted() {
    use shepherd_management::AdminAuthority;
    use std::sync::Mutex;

    struct Roster(Mutex<Vec<String>>);
    impl AdminAuthority for Roster {
        fn verify_http_token(&self, presented: &str) -> bool {
            self.0.lock().unwrap().iter().any(|t| t == presented)
        }
        fn has_admin(&self) -> bool {
            !self.0.lock().unwrap().is_empty()
        }
    }

    let cfg = temp_config();
    let roster = Arc::new(Roster(Mutex::new(vec![
        "first-phone".to_string(),
        "second-phone".to_string(),
    ])));
    let admin: Arc<dyn AdminAuthority> = roster.clone();
    let app = make_app_with_admin(None, cfg.path().to_path_buf(), Some(admin));

    for token in ["first-phone", "second-phone"] {
        let (status, _) = rpc_auth(&app, "health", json!({}), token).await;
        assert_eq!(status, StatusCode::OK, "{token} should be accepted");
    }
    let (status, _) = rpc_auth(&app, "health", json!({}), "third-phone").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Revoke the second phone. The first keeps working; the second does not.
    roster.0.lock().unwrap().retain(|t| t != "second-phone");
    let (status, _) = rpc_auth(&app, "health", json!({}), "first-phone").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = rpc_auth(&app, "health", json!({}), "second-phone").await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn auth_admin_authority_unclaimed_stays_open() {
    use shepherd_management::AdminAuthority;

    // BLE-derived authority is plumbed in but no admin has claimed
    // yet (the default state on a fresh install). Without a static
    // token configured this must remain open mode — otherwise a
    // fresh install with BLE enabled locks itself out of HTTP
    // before any admin has been set up.
    struct UnclaimedAdmin;
    impl AdminAuthority for UnclaimedAdmin {
        fn verify_http_token(&self, _presented: &str) -> bool {
            false
        }
        fn has_admin(&self) -> bool {
            false
        }
    }

    let cfg = temp_config();
    let admin: Arc<dyn AdminAuthority> = Arc::new(UnclaimedAdmin);
    let app = make_app_with_admin(None, cfg.path().to_path_buf(), Some(admin));

    let (status, _) = rpc(&app, "health", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = rpc_auth(&app, "health", json!({}), "anything").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn auth_static_and_admin_tokens_both_accepted() {
    use shepherd_management::AdminAuthority;
    use std::sync::Mutex;

    // A roster, not a single token: a device can have several administrators
    // and each has their own (issue #149).
    struct MockAdmin(Mutex<Vec<String>>);
    impl AdminAuthority for MockAdmin {
        fn verify_http_token(&self, presented: &str) -> bool {
            self.0.lock().unwrap().iter().any(|t| t == presented)
        }
        fn has_admin(&self) -> bool {
            !self.0.lock().unwrap().is_empty()
        }
    }

    let cfg = temp_config();
    let admin: Arc<dyn AdminAuthority> =
        Arc::new(MockAdmin(Mutex::new(vec!["admin-tok".to_string()])));
    let app = make_app_with_admin(Some("static"), cfg.path().to_path_buf(), Some(admin));

    let (status, _) = rpc_auth(&app, "health", json!({}), "static").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = rpc_auth(&app, "health", json!({}), "admin-tok").await;
    assert_eq!(status, StatusCode::OK);
}

// ---------------------------------------------------------------------------
// Status-code mapping
//
// One representative call per arm of `management_error_to_http` and the
// `RpcDispatchError` match in `handlers::rpc::dispatch`. The method
// behaviors themselves are covered in the shared-layer dispatch tests;
// here we only assert the HTTP status + `error` code they map onto.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ok_result_returns_200_with_body() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = rpc(&app, "health", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["live"], true);
}

#[tokio::test]
async fn unknown_method_returns_404_method_not_found() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = rpc(&app, "no_such_method", json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "method_not_found");
}

#[tokio::test]
async fn invalid_params_returns_400() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    // `upsert_override` requires an `id`, so `{}` fails param parsing.
    let (status, body) = rpc(&app, "upsert_override", json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "invalid_params");
}

#[tokio::test]
async fn management_not_found_returns_404() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = rpc(&app, "get_entry", json!({ "id": "does-not-exist" })).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "not_found");
}

#[tokio::test]
async fn management_forbidden_returns_403() {
    let mut policy = test_policy();
    policy.volume = VolumePolicy {
        max_volume: Some(100),
        min_volume: Some(0),
        allow_mute: true,
        allow_change: false,
    };
    let cfg = temp_config();
    let app = make_app_with_policy(policy, None, cfg.path().to_path_buf());
    let (status, body) = rpc(&app, "set_volume", json!({ "percent": 75 })).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "forbidden");
}

#[tokio::test]
async fn management_unprocessable_returns_422() {
    let cfg = NamedTempFile::new().unwrap();
    std::fs::write(cfg.path(), "not = valid = toml").unwrap();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = rpc(&app, "reload_config", json!({})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "unprocessable");
}

// ---------------------------------------------------------------------------
// Web management authentication (issue #156)
//
// The store's own behaviour — hashing, expiry, lockout arithmetic — is tested
// in `shepherd-management/src/webauth.rs`. What is left here is what only the
// HTTP layer can get wrong: which routes are reachable without a credential,
// whether the cookie carries the right attributes, and whether a cookie-authed
// cross-origin write is refused.
// ---------------------------------------------------------------------------

fn web_store(dir: &tempfile::TempDir) -> Arc<shepherd_management::WebAuth> {
    Arc::new(
        shepherd_management::WebAuth::load(
            Arc::new(shepherd_util::LocalProtectedFiles::new(
                dir.path().to_path_buf(),
            )),
            shepherd_management::WebAuthPolicy::default(),
        )
        .expect("store loads"),
    )
}

/// A router with a credential store, as a device has.
fn make_web_app(
    web: Arc<shepherd_management::WebAuth>,
    config_path: PathBuf,
    auth_token: Option<&str>,
) -> axum::Router {
    make_app_full(
        test_policy(),
        auth_token,
        config_path,
        None,
        Some(web),
        false,
    )
}

fn post(uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn get_with_cookie(uri: &str, cookie: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header(header::COOKIE, cookie)
        .body(Body::empty())
        .unwrap()
}

/// Send a request and hand back the status, the body, and any `Set-Cookie`.
async fn send_full(app: &axum::Router, req: Request<Body>) -> (StatusCode, Value, Option<String>) {
    let response = app.clone().oneshot(req).await.unwrap();
    let status = response.status();
    let cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let json = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, json, cookie)
}

/// Complete first-run setup and return the session cookie to present.
async fn enrol(app: &axum::Router, web: &shepherd_management::WebAuth) -> String {
    let code = web.enrolment_code().expect("a fresh store has a code");
    let (status, _, cookie) = send_full(
        app,
        post(
            "/api/v1/auth/setup",
            json!({ "code": code, "password": "correct horse battery" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let cookie = cookie.expect("setup sets a session cookie");
    cookie.split(';').next().unwrap().to_string()
}

/// A management API built without a credential store must not come up.
///
/// This is the invariant the whole of issue #156 rests on, and until now
/// nothing checked it: `shepherdd` built a store next to the server and the
/// two were correct only because they were written that way. `with_web_auth`
/// now takes a store rather than an `Option`, so forgetting the call is the
/// only way left to get here — and `run` turns that into a daemon that
/// refuses to start rather than one that starts open.
#[tokio::test]
async fn a_server_with_no_credential_store_refuses_to_serve() {
    let cfg = temp_config();
    let state = make_state(test_policy(), cfg.path().to_path_buf());
    let api_cfg = shepherd_config::ManagementApiConfig {
        // Port 0 would still be a real bind; the check has to fire before it,
        // so a misassembled server never holds the port at all.
        port: 0,
        bind: std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        bind_retry: Some(std::time::Duration::from_secs(1)),
        auth_token: Some("a machine token is not a substitute".into()),
        tls: shepherd_config::TlsMode::Off,
        auth: shepherd_config::WebAuthLimits::default(),
    };
    let (_tx, rx) = watch::channel(false);
    // Bounded, because the regression this guards against does not fail --
    // it *serves*. Without the check `run` binds and then never returns, so an
    // unbounded await would hang CI rather than report anything.
    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        shepherd_http::HttpServer::new(state, api_cfg).run(rx),
    )
    .await
    .expect("a server with no credential store must refuse rather than serve");
    let err = outcome.expect_err("a server with no credential store must not serve");
    let message = format!("{err:#}");
    assert!(
        message.contains("credential store"),
        "the error has to name the reason: {message}"
    );
}

#[tokio::test]
async fn an_unconfigured_device_answers_status_and_refuses_everything_else() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = temp_config();
    let web = web_store(&dir);
    let app = make_web_app(web.clone(), cfg.path().to_path_buf(), None);

    // The login page has to be able to render, so `status` is reachable.
    let (status, body) = send(
        &app,
        Request::builder()
            .uri("/api/v1/auth/status")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["configured"], json!(false));

    // But nothing else is. This is the replacement for open mode: an
    // unconfigured device used to serve the whole management surface to
    // anyone who could reach the port.
    let (status, _) = rpc(&app, "health", json!({})).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn setup_then_the_cookie_authenticates() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = temp_config();
    let web = web_store(&dir);
    let app = make_web_app(web.clone(), cfg.path().to_path_buf(), None);
    let cookie = enrol(&app, &web).await;

    let (status, _) = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/v1/rpc")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::COOKIE, &cookie)
            .body(Body::from(
                serde_json::to_vec(&json!({"method": "health", "params": {}})).unwrap(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn the_session_cookie_is_httponly_and_samesite_strict() {
    // The two attributes that do the work: `HttpOnly` is why moving off
    // `localStorage` was worth doing, and `SameSite=Strict` is most of the
    // CSRF answer.
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = temp_config();
    let web = web_store(&dir);
    let app = make_web_app(web.clone(), cfg.path().to_path_buf(), None);
    let code = web.enrolment_code().unwrap();
    let (_, _, cookie) = send_full(
        &app,
        post(
            "/api/v1/auth/setup",
            json!({ "code": code, "password": "correct horse battery" }),
        ),
    )
    .await;
    let cookie = cookie.expect("a cookie");
    assert!(cookie.contains("HttpOnly"), "{cookie}");
    assert!(cookie.contains("SameSite=Strict"), "{cookie}");
    assert!(cookie.contains("Path=/"), "{cookie}");
    // Not `Secure` here: this router is plaintext, and a `Secure` cookie on a
    // plaintext origin is simply dropped, which would lock the browser out.
    assert!(!cookie.contains("Secure"), "{cookie}");
}

#[tokio::test]
async fn a_tls_listener_marks_the_cookie_secure() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = temp_config();
    let web = web_store(&dir);
    let app = make_app_full(
        test_policy(),
        None,
        cfg.path().to_path_buf(),
        None,
        Some(web.clone()),
        true,
    );
    let code = web.enrolment_code().unwrap();
    let (_, _, cookie) = send_full(
        &app,
        post(
            "/api/v1/auth/setup",
            json!({ "code": code, "password": "correct horse battery" }),
        ),
    )
    .await;
    assert!(cookie.expect("a cookie").contains("Secure"));
}

#[tokio::test]
async fn a_wrong_setup_code_is_403_and_sets_no_cookie() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = temp_config();
    let web = web_store(&dir);
    let app = make_web_app(web, cfg.path().to_path_buf(), None);
    let (status, body, cookie) = send_full(
        &app,
        post(
            "/api/v1/auth/setup",
            json!({ "code": "000000", "password": "correct horse battery" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "forbidden");
    assert!(cookie.is_none());
}

#[tokio::test]
async fn a_short_password_is_422_with_the_reason() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = temp_config();
    let web = web_store(&dir);
    let code = web.enrolment_code().unwrap();
    let app = make_web_app(web, cfg.path().to_path_buf(), None);
    let (status, body, _) = send_full(
        &app,
        post(
            "/api/v1/auth/setup",
            json!({ "code": code, "password": "short" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        body["message"].as_str().unwrap().contains("8"),
        "the message should say how long: {body}"
    );
}

#[tokio::test]
async fn signing_out_clears_the_cookie_and_the_session() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = temp_config();
    let web = web_store(&dir);
    let app = make_web_app(web.clone(), cfg.path().to_path_buf(), None);
    let cookie = enrol(&app, &web).await;

    let (status, _, set_cookie) = send_full(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/v1/auth/signout")
            .header(header::COOKIE, &cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(set_cookie.expect("a clearing cookie").contains("Max-Age=0"));

    // And the session is actually gone, not merely forgotten by the browser.
    let (status, _) = send(&app, get_with_cookie("/api/v1/auth/session", &cookie)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_machine_token_authenticates_but_is_not_a_session() {
    // The demotion decision, in one test: the static token still works on a
    // request and still cannot be a person logged in.
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = temp_config();
    let web = web_store(&dir);
    let app = make_web_app(web, cfg.path().to_path_buf(), Some("machine-token"));

    let (status, _) = rpc_auth(&app, "health", json!({}), "machine-token").await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = send(
        &app,
        Request::builder()
            .uri("/api/v1/auth/session")
            .header(header::AUTHORIZATION, "Bearer machine-token")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["machine"], json!(true));
    assert_eq!(body["session"], Value::Null);
}

#[tokio::test]
async fn a_session_token_also_works_as_a_bearer() {
    // What the web UI's cross-origin "API Server URL" mode needs: a cookie set
    // by one origin is not sent to another, so the same session has to be
    // presentable as a bearer.
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = temp_config();
    let web = web_store(&dir);
    let app = make_web_app(web.clone(), cfg.path().to_path_buf(), None);
    let cookie = enrol(&app, &web).await;
    let token = cookie.split('=').nth(1).unwrap().to_string();

    let (status, _) = rpc_auth(&app, "health", json!({}), &token).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn a_cookie_authed_cross_origin_write_is_refused() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = temp_config();
    let web = web_store(&dir);
    let app = make_web_app(web.clone(), cfg.path().to_path_buf(), None);
    let cookie = enrol(&app, &web).await;

    let (status, body) = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/v1/rpc")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::COOKIE, &cookie)
            .header(header::HOST, "shepherd.local:7890")
            .header(header::ORIGIN, "https://evil.example")
            .body(Body::from(
                serde_json::to_vec(&json!({"method": "health", "params": {}})).unwrap(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "forbidden");
}

#[tokio::test]
async fn a_same_origin_write_with_an_origin_header_is_allowed() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = temp_config();
    let web = web_store(&dir);
    let app = make_web_app(web.clone(), cfg.path().to_path_buf(), None);
    let cookie = enrol(&app, &web).await;

    let (status, _) = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/v1/rpc")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::COOKIE, &cookie)
            .header(header::HOST, "shepherd.local:7890")
            .header(header::ORIGIN, "https://shepherd.local:7890")
            .body(Body::from(
                serde_json::to_vec(&json!({"method": "health", "params": {}})).unwrap(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn a_bearer_client_is_not_subject_to_the_origin_check() {
    // Nothing attaches a bearer header automatically, so a cross-site page
    // cannot forge one — and a script that legitimately sets `Origin` should
    // not be locked out.
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = temp_config();
    let web = web_store(&dir);
    let app = make_web_app(web, cfg.path().to_path_buf(), Some("machine-token"));

    let (status, _) = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/v1/rpc")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, "Bearer machine-token")
            .header(header::HOST, "shepherd.local:7890")
            .header(header::ORIGIN, "https://elsewhere.example")
            .body(Body::from(
                serde_json::to_vec(&json!({"method": "health", "params": {}})).unwrap(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn the_sessions_list_marks_the_caller_and_revoking_another_leaves_it_alone() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = temp_config();
    let web = web_store(&dir);
    let app = make_web_app(web.clone(), cfg.path().to_path_buf(), None);
    let cookie = enrol(&app, &web).await;

    // A second session, as if from another device.
    let (_, _, other_cookie) = send_full(
        &app,
        post(
            "/api/v1/auth/login",
            json!({ "password": "correct horse battery" }),
        ),
    )
    .await;
    let other_cookie = other_cookie.unwrap().split(';').next().unwrap().to_string();

    let (status, body) = send(&app, get_with_cookie("/api/v1/auth/sessions", &cookie)).await;
    assert_eq!(status, StatusCode::OK);
    let sessions = body.as_array().expect("an array");
    assert_eq!(sessions.len(), 2);
    let current: Vec<&Value> = sessions
        .iter()
        .filter(|s| s["current"] == json!(true))
        .collect();
    assert_eq!(current.len(), 1);

    // Revoke the *other* one and stay signed in.
    let other_id = sessions
        .iter()
        .find(|s| s["current"] == json!(false))
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (status, _) = send(
        &app,
        Request::builder()
            .method("DELETE")
            .uri(format!("/api/v1/auth/sessions/{other_id}"))
            .header(header::COOKIE, &cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _) = send(&app, get_with_cookie("/api/v1/auth/session", &cookie)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(&app, get_with_cookie("/api/v1/auth/session", &other_cookie)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn revoking_your_own_session_clears_your_cookie_too() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = temp_config();
    let web = web_store(&dir);
    let app = make_web_app(web.clone(), cfg.path().to_path_buf(), None);
    let cookie = enrol(&app, &web).await;
    let (_, body) = send(&app, get_with_cookie("/api/v1/auth/sessions", &cookie)).await;
    let id = body[0]["id"].as_str().unwrap().to_string();

    let (status, _, set_cookie) = send_full(
        &app,
        Request::builder()
            .method("DELETE")
            .uri(format!("/api/v1/auth/sessions/{id}"))
            .header(header::COOKIE, &cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(
        set_cookie.expect("a clearing cookie").contains("Max-Age=0"),
        "revoking your own session should not leave the browser holding a dead cookie"
    );
}

#[tokio::test]
async fn a_companion_approval_signs_the_browser_in() {
    // The whole handshake over HTTP: the browser asks, the administrator
    // approves through the RPC the companion uses, the browser's next poll
    // comes back with a session cookie.
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = temp_config();
    let web = web_store(&dir);
    let app = make_web_app(web.clone(), cfg.path().to_path_buf(), Some("machine-token"));
    let code = web.enrolment_code().unwrap();
    send_full(
        &app,
        post(
            "/api/v1/auth/setup",
            json!({ "code": code, "password": "correct horse battery" }),
        ),
    )
    .await;

    let (status, body, _) = send_full(&app, post("/api/v1/auth/request", json!({}))).await;
    assert_eq!(status, StatusCode::OK);
    let poll_token = body["poll_token"].as_str().unwrap().to_string();
    let shown_code = body["code"].as_str().unwrap().to_string();

    let (status, pending) = rpc_auth(&app, "list_login_requests", json!({}), "machine-token").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(pending[0]["code"], json!(shown_code));

    let (status, answer, cookie) = send_full(
        &app,
        post("/api/v1/auth/poll", json!({ "poll_token": poll_token })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(answer["state"], json!("pending"));
    assert!(cookie.is_none());

    let id = pending[0]["id"].as_str().unwrap().to_string();
    let (status, _) = rpc_auth(
        &app,
        "approve_login_request",
        json!({ "id": id }),
        "machine-token",
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, answer, cookie) = send_full(
        &app,
        post("/api/v1/auth/poll", json!({ "poll_token": poll_token })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(answer["state"], json!("approved"));
    let cookie = cookie.expect("an approved poll sets the session cookie");
    let cookie = cookie.split(';').next().unwrap().to_string();

    let (status, _) = send(&app, get_with_cookie("/api/v1/auth/session", &cookie)).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn a_denied_request_tells_the_browser_and_hands_out_nothing() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = temp_config();
    let web = web_store(&dir);
    let app = make_web_app(web.clone(), cfg.path().to_path_buf(), Some("machine-token"));
    let code = web.enrolment_code().unwrap();
    send_full(
        &app,
        post(
            "/api/v1/auth/setup",
            json!({ "code": code, "password": "correct horse battery" }),
        ),
    )
    .await;

    let (_, body, _) = send_full(&app, post("/api/v1/auth/request", json!({}))).await;
    let poll_token = body["poll_token"].as_str().unwrap().to_string();
    let (_, pending) = rpc_auth(&app, "list_login_requests", json!({}), "machine-token").await;
    let id = pending[0]["id"].as_str().unwrap().to_string();
    let (status, _) = rpc_auth(
        &app,
        "deny_login_request",
        json!({ "id": id }),
        "machine-token",
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, answer, cookie) = send_full(
        &app,
        post("/api/v1/auth/poll", json!({ "poll_token": poll_token })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(answer["state"], json!("denied"));
    assert!(cookie.is_none());
}

#[tokio::test]
async fn lockout_answers_429_with_retry_after() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = temp_config();
    let web = Arc::new(
        shepherd_management::WebAuth::load(
            Arc::new(shepherd_util::LocalProtectedFiles::new(
                dir.path().to_path_buf(),
            )),
            shepherd_management::WebAuthPolicy {
                lockout_after: 2,
                lockout: Duration::from_secs(60),
                ..Default::default()
            },
        )
        .unwrap(),
    );
    let app = make_web_app(web.clone(), cfg.path().to_path_buf(), None);
    let code = web.enrolment_code().unwrap();
    send_full(
        &app,
        post(
            "/api/v1/auth/setup",
            json!({ "code": code, "password": "correct horse battery" }),
        ),
    )
    .await;

    for _ in 0..2 {
        let (status, _, _) = send_full(
            &app,
            post(
                "/api/v1/auth/login",
                json!({ "password": "wrong password!" }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    let response = app
        .clone()
        .oneshot(post(
            "/api/v1/auth/login",
            json!({ "password": "correct horse battery" }),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    let retry_after = response
        .headers()
        .get(header::RETRY_AFTER)
        .expect("a lockout says how long to wait")
        .to_str()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    assert!((1..=60).contains(&retry_after), "got {retry_after}");
}

#[tokio::test]
async fn a_malformed_login_body_is_400_not_500() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = temp_config();
    let web = web_store(&dir);
    let app = make_web_app(web, cfg.path().to_path_buf(), None);
    let (status, body) = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/v1/auth/login")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{not json"))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "bad_request");
}

#[tokio::test]
async fn a_stale_cookie_falls_through_to_unauthorized_rather_than_erroring() {
    let dir = tempfile::TempDir::new().unwrap();
    let cfg = temp_config();
    let web = web_store(&dir);
    let app = make_web_app(web, cfg.path().to_path_buf(), None);
    let (status, _) = send(
        &app,
        get_with_cookie("/api/v1/auth/session", "shepherd_session=long-gone"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// The policy file (issue #185)
//
// `GET`/`PUT /api/v1/config` are the config editor's end of the device. They
// are not RPC methods -- see `handlers/config.rs` for why -- so they need
// their own coverage of the things the RPC endpoint's tests establish once:
// that the auth layer covers them, and how a `ManagementError` reaches the
// wire.
// ---------------------------------------------------------------------------

/// A valid single-entry policy, as an editor would send it.
const ONE_ENTRY: &str = "config_version = 1\n\n[[entries]]\nid = \"a\"\nlabel = \"A\"\n\n\
                         [entries.kind]\ntype = \"process\"\ncommand = \"/bin/true\"\n";

/// Send a request and hand back the status, the `ETag`, and the body as text.
async fn send_raw(app: &axum::Router, req: Request<Body>) -> (StatusCode, Option<String>, String) {
    let response = app.clone().oneshot(req).await.unwrap();
    let status = response.status();
    let etag = response
        .headers()
        .get(header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, etag, String::from_utf8_lossy(&bytes).to_string())
}

fn get_config() -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri("/api/v1/config")
        .body(Body::empty())
        .unwrap()
}

fn put_config(body: &str, if_match: Option<&str>) -> Request<Body> {
    let mut b = Request::builder()
        .method("PUT")
        .uri("/api/v1/config")
        .header(header::CONTENT_TYPE, "text/plain")
        // Same-origin, so the CSRF check has nothing to object to.
        .header(header::HOST, "device.local")
        .header(header::ORIGIN, "https://device.local");
    if let Some(tag) = if_match {
        b = b.header(header::IF_MATCH, tag);
    }
    b.body(Body::from(body.to_string())).unwrap()
}

#[tokio::test]
async fn config_get_returns_the_file_and_an_etag() {
    let cfg = temp_config();
    std::fs::write(cfg.path(), ONE_ENTRY).unwrap();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, etag, body) = send_raw(&app, get_config()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, ONE_ENTRY);
    assert!(etag.is_some_and(|t| t.starts_with('"')), "quoted ETag");
}

#[tokio::test]
async fn config_put_without_if_match_is_refused() {
    // Forgetting the precondition must not be the same as overwriting: on a
    // device the other writer is a person at a terminal.
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, _, _) = send_raw(&app, put_config(ONE_ENTRY, None)).await;
    assert_eq!(status, StatusCode::PRECONDITION_REQUIRED);
    assert_eq!(
        std::fs::read_to_string(cfg.path()).unwrap(),
        "config_version = 1\n"
    );
}

#[tokio::test]
async fn config_put_round_trips_through_its_own_etag() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (_, etag, _) = send_raw(&app, get_config()).await;
    let (status, new_etag, _) = send_raw(&app, put_config(ONE_ENTRY, etag.as_deref())).await;
    assert_eq!(status, StatusCode::OK);
    assert_ne!(new_etag, etag);
    assert_eq!(std::fs::read_to_string(cfg.path()).unwrap(), ONE_ENTRY);
}

#[tokio::test]
async fn config_put_with_a_stale_etag_is_412() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, _, body) = send_raw(
        &app,
        put_config(ONE_ENTRY, Some("\"0123456789abcdef0123456789abcdef\"")),
    )
    .await;
    assert_eq!(status, StatusCode::PRECONDITION_FAILED);
    assert!(body.contains("changed since"), "{body}");
    assert_eq!(
        std::fs::read_to_string(cfg.path()).unwrap(),
        "config_version = 1\n"
    );
}

#[tokio::test]
async fn config_put_with_a_star_overwrites() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, _, _) = send_raw(&app, put_config(ONE_ENTRY, Some("*"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(std::fs::read_to_string(cfg.path()).unwrap(), ONE_ENTRY);
}

#[tokio::test]
async fn config_put_that_does_not_parse_is_422_and_changes_nothing() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, _, body) = send_raw(&app, put_config("not = valid = toml", Some("*"))).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body_error(&body), "unprocessable");
    assert_eq!(
        std::fs::read_to_string(cfg.path()).unwrap(),
        "config_version = 1\n"
    );
}

#[tokio::test]
async fn config_routes_are_behind_the_auth_layer() {
    let cfg = temp_config();
    let app = make_app(Some("secret"), cfg.path().to_path_buf());
    let (status, _, _) = send_raw(&app, get_config()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, _) = send_raw(&app, put_config(ONE_ENTRY, Some("*"))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// The `error` field of a JSON error body.
fn body_error(body: &str) -> String {
    serde_json::from_str::<Value>(body).unwrap()["error"]
        .as_str()
        .unwrap()
        .to_string()
}
