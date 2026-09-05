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
use shepherd_management::{AutoBrightnessState, DefaultManagementService};
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
        media_refresh_tx: None,
        shutdown_tx,
        hidpi: Arc::new(shepherd_host_api::NoOpHidpiController),
        hud_layout: Arc::new(shepherd_host_api::NoOpHudLayoutController),
        display: Arc::new(shepherd_host_api::NoOpDisplayController),
        last_audio_state: Arc::new(tokio::sync::Mutex::new(None)),
        diagnostics: None,
    });
    let state = AppState { svc };
    handlers::router(
        state,
        shepherd_http::AuthSources {
            static_token: auth_token.map(str::to_owned),
            admin,
        },
    )
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

    struct MockAdmin(Mutex<Option<String>>);
    impl AdminAuthority for MockAdmin {
        fn current_http_token(&self) -> Option<String> {
            self.0.lock().unwrap().clone()
        }
    }

    let cfg = temp_config();
    let admin: Arc<dyn AdminAuthority> =
        Arc::new(MockAdmin(Mutex::new(Some("admin-tok".to_string()))));
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
        fn current_http_token(&self) -> Option<String> {
            None
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

    struct MockAdmin(Mutex<Option<String>>);
    impl AdminAuthority for MockAdmin {
        fn current_http_token(&self) -> Option<String> {
            self.0.lock().unwrap().clone()
        }
    }

    let cfg = temp_config();
    let admin: Arc<dyn AdminAuthority> =
        Arc::new(MockAdmin(Mutex::new(Some("admin-tok".to_string()))));
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
