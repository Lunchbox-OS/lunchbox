//! Integration tests for the shepherd-http management API.
//!
//! Every operation on the HTTP surface goes through the single
//! `POST /api/v1/rpc` endpoint (see `handlers/rpc.rs`) — the REST
//! routes are gone. Tests here exercise that endpoint against a
//! fully-wired-up `AppState` with mocked host/volume/brightness.

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use shepherd_api::{EntryKind, Event};
use shepherd_config::{
    AvailabilityPolicy, BrightnessPolicy, Entry, LimitsPolicy, Policy, ServiceConfig, VolumePolicy,
};
use shepherd_core::CoreEngine;
use shepherd_host_api::{
    BrightnessCapabilities, BrightnessController, BrightnessResult, BrightnessStatus,
    HostCapabilities, MockHost, VolumeCapabilities, VolumeController, VolumeResult, VolumeStatus,
};
use shepherd_http::{AppState, handlers};
use shepherd_management::DefaultManagementService;
use shepherd_store::SqliteStore;
use shepherd_util::EntryId;
use shepherd_util::{DaysOfWeek, TimeWindow, WallClock};
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
            xwayland_native_resolution: false,
            confirm_on_close: false,
        }],
        default_warnings: vec![],
        default_max_run: Some(Duration::from_secs(3600)),
        volume: VolumePolicy::unrestricted(),
        brightness: BrightnessPolicy::default(),
    }
}

fn make_app_with_policy(
    policy: Policy,
    auth_token: Option<&str>,
    config_path: PathBuf,
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
        event_tx: tx,
        broadcast_fn: Arc::new(move |event: Event| {
            let _ = tx_for_fn.send(event);
        }),
        config_path,
        shutdown_tx,
        hidpi: Arc::new(shepherd_host_api::NoOpHidpiController),
    });
    let state = AppState { svc };
    handlers::router(
        state,
        shepherd_http::AuthSources {
            static_token: auth_token.map(str::to_owned),
            admin: None,
        },
    )
}

fn make_app(auth_token: Option<&str>, config_path: PathBuf) -> axum::Router {
    make_app_with_policy(test_policy(), auth_token, config_path)
}

fn make_app_with_admin(
    auth_token: Option<&str>,
    config_path: PathBuf,
    admin: Option<Arc<dyn shepherd_management::AdminAuthority>>,
) -> axum::Router {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let host = Arc::new(MockHost::new());
    let volume = Arc::new(MockVolume::new());
    let brightness = Arc::new(MockBrightness::new());
    let engine = Arc::new(Mutex::new(CoreEngine::new(
        test_policy(),
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
        event_tx: tx,
        broadcast_fn: Arc::new(move |event: Event| {
            let _ = tx_for_fn.send(event);
        }),
        config_path,
        shutdown_tx,
        hidpi: Arc::new(shepherd_host_api::NoOpHidpiController),
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
// Health / state
// ---------------------------------------------------------------------------

#[tokio::test]
async fn health_returns_ok() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = rpc(&app, "health", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["live"], true);
    assert_eq!(body["ready"], true);
}

#[tokio::test]
async fn service_state_returns_snapshot() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = rpc(&app, "service_state", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["api_version"].is_number());
    assert!(body["current_session"].is_null());
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
// Entries
// ---------------------------------------------------------------------------

#[tokio::test]
async fn entries_list_returns_all_entries() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = rpc(&app, "list_entries", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["entry_id"], "test-game");
    assert_eq!(arr[0]["enabled"], true);
}

#[tokio::test]
async fn entry_get_known_returns_200() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = rpc(&app, "get_entry", json!({ "id": "test-game" })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["entry_id"], "test-game");
    assert_eq!(body["label"], "Test Game");
}

#[tokio::test]
async fn entry_get_unknown_returns_404() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = rpc(&app, "get_entry", json!({ "id": "does-not-exist" })).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "not_found");
}

#[tokio::test]
async fn unknown_method_returns_404() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = rpc(&app, "no_such_method", json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "method_not_found");
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn current_session_is_null_initially() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = rpc(&app, "current_session", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_null());
}

#[tokio::test]
async fn launch_known_entry_is_approved() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = rpc(&app, "launch", json!({ "id": "test-game" })).await;
    assert_eq!(status, StatusCode::OK);
    // LaunchOutcome is serde's externally-tagged form.
    assert!(body["Approved"].is_object());
    assert!(body["Approved"]["session_id"].is_string());
}

#[tokio::test]
async fn launch_unknown_entry_is_denied() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = rpc(&app, "launch", json!({ "id": "no-such-entry" })).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["Denied"].is_object());
    assert!(body["Denied"]["reasons"].is_array());
}

#[tokio::test]
async fn stop_current_no_active_session_returns_404() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = rpc(&app, "stop_current", json!({})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "not_found");
}

#[tokio::test]
async fn extend_current_no_active_session_returns_404() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, _) = rpc(&app, "extend_current", json!({ "seconds": 60 })).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn sessions_full_lifecycle() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());

    // No session yet
    let (status, body) = rpc(&app, "current_session", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_null());

    // Launch
    let (status, body) = rpc(&app, "launch", json!({ "id": "test-game" })).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["Approved"].is_object());

    // Session is active
    let (status, body) = rpc(&app, "current_session", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.is_null());
    assert_eq!(body["entry_id"], "test-game");

    // Second launch is denied (session already active)
    let (status, body) = rpc(&app, "launch", json!({ "id": "test-game" })).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["Denied"].is_object());

    // Stop
    let (status, _) = rpc(&app, "stop_current", json!({})).await;
    assert_eq!(status, StatusCode::OK);

    // Session is null again
    let (status, body) = rpc(&app, "current_session", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_null());
}

#[tokio::test]
async fn extend_active_session() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());

    let (_, _) = rpc(&app, "launch", json!({ "id": "test-game" })).await;
    let (status, body) = rpc(&app, "extend_current", json!({ "seconds": 120 })).await;
    assert_eq!(status, StatusCode::OK);
    // wrap_result = "new_deadline"
    assert!(body["new_deadline"].is_string() || body["new_deadline"].is_null());
}

#[tokio::test]
async fn reduce_active_session() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());

    let (_, _) = rpc(&app, "launch", json!({ "id": "test-game" })).await;
    let (status, body) = rpc(&app, "extend_current", json!({ "seconds": -60 })).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["new_deadline"].is_string() || body["new_deadline"].is_null());
}

// ---------------------------------------------------------------------------
// Overrides
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_overrides_empty_initially() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = rpc(&app, "list_overrides", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn upsert_override_creates_record() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = rpc(
        &app,
        "upsert_override",
        json!({ "id": "test-game", "availability": true }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["entry_id"], "test-game");
    assert_eq!(body["availability"], true);
}

#[tokio::test]
async fn get_override_returns_existing_record() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let _ = rpc(
        &app,
        "upsert_override",
        json!({ "id": "test-game", "quota_delta_seconds": 300 }),
    )
    .await;
    let (status, body) = rpc(&app, "get_override", json!({ "id": "test-game" })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["quota_delta_seconds"], 300);
}

#[tokio::test]
async fn get_override_nonexistent_returns_null() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = rpc(&app, "get_override", json!({ "id": "test-game" })).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_null());
}

#[tokio::test]
async fn upsert_override_missing_id_returns_400() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = rpc(&app, "upsert_override", json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "invalid_params");
}

#[tokio::test]
async fn list_overrides_shows_created_record() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let _ = rpc(
        &app,
        "upsert_override",
        json!({ "id": "test-game", "availability": true }),
    )
    .await;
    let (status, body) = rpc(&app, "list_overrides", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn delete_override_existing_returns_true() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let _ = rpc(
        &app,
        "upsert_override",
        json!({ "id": "test-game", "availability": true }),
    )
    .await;
    let (status, body) = rpc(&app, "delete_override", json!({ "id": "test-game" })).await;
    assert_eq!(status, StatusCode::OK);
    // wrap_result = "deleted"
    assert_eq!(body["deleted"], true);
}

#[tokio::test]
async fn delete_override_nonexistent_returns_false() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = rpc(&app, "delete_override", json!({ "id": "test-game" })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["deleted"], false);
}

// ---------------------------------------------------------------------------
// Usage
// ---------------------------------------------------------------------------

#[tokio::test]
async fn usage_all_returns_empty_without_sessions() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = rpc(&app, "usage_all", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_array());
}

#[tokio::test]
async fn usage_entry_returns_empty_for_known_entry() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = rpc(&app, "usage_entry", json!({ "id": "test-game" })).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_array());
}

// ---------------------------------------------------------------------------
// Volume
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_volume_returns_status() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = rpc(&app, "get_volume", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["percent"], 50);
    assert_eq!(body["muted"], false);
}

#[tokio::test]
async fn set_volume_updates_percent() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = rpc(&app, "set_volume", json!({ "percent": 75 })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["percent"], 75);
}

#[tokio::test]
async fn set_mute_updates_muted() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = rpc(&app, "set_mute", json!({ "muted": true })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["muted"], true);
}

#[tokio::test]
async fn set_volume_above_max_is_clamped() {
    let mut policy = test_policy();
    policy.volume = VolumePolicy {
        max_volume: Some(60),
        min_volume: Some(0),
        allow_mute: true,
        allow_change: true,
    };
    let cfg = temp_config();
    let app = make_app_with_policy(policy, None, cfg.path().to_path_buf());
    let (status, body) = rpc(&app, "set_volume", json!({ "percent": 95 })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["percent"], 60);
}

#[tokio::test]
async fn set_volume_forbidden_when_change_disallowed() {
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
async fn set_mute_forbidden_when_mute_disallowed() {
    let mut policy = test_policy();
    policy.volume = VolumePolicy {
        max_volume: Some(100),
        min_volume: Some(0),
        allow_mute: false,
        allow_change: true,
    };
    let cfg = temp_config();
    let app = make_app_with_policy(policy, None, cfg.path().to_path_buf());
    let (status, body) = rpc(&app, "set_mute", json!({ "muted": true })).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "forbidden");
}

#[tokio::test]
async fn volume_up_and_down_walk_through_step() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = rpc(&app, "volume_up", json!({ "step": 10 })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["percent"], 60);

    let (status, body) = rpc(&app, "volume_down", json!({ "step": 25 })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["percent"], 35);
}

#[tokio::test]
async fn toggle_mute_flips_state() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = rpc(&app, "toggle_mute", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["muted"], true);
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reload_config_valid_file_returns_ok() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = rpc(&app, "reload_config", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    // wrap_result = "entry_count"
    assert!(body["entry_count"].is_number());
}

#[tokio::test]
async fn reload_config_invalid_file_returns_422() {
    let cfg = NamedTempFile::new().unwrap();
    std::fs::write(cfg.path(), "not = valid = toml").unwrap();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = rpc(&app, "reload_config", json!({})).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "unprocessable");
}

// ---------------------------------------------------------------------------
// Ping
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ping_returns_null() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = rpc(&app, "ping", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_null());
}

// ---------------------------------------------------------------------------
// Overrides / policy interactions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn disabled_entry_via_override_shows_as_unavailable() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());

    // Disable for today via override
    let _ = rpc(
        &app,
        "upsert_override",
        json!({ "id": "test-game", "availability": false }),
    )
    .await;

    let (status, body) = rpc(&app, "get_entry", json!({ "id": "test-game" })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enabled"], false);
}

#[tokio::test]
async fn enable_entry_outside_time_window() {
    // Entry with a lunch-only window that we enable "today" via
    // override — the override should override the time window at
    // the policy level so `get_entry` reports it available.
    let mut policy = test_policy();
    policy.entries[0].availability = AvailabilityPolicy {
        windows: vec![TimeWindow {
            days: DaysOfWeek::new(0x7F),
            start: WallClock::new(11, 0).unwrap(),
            end: WallClock::new(12, 0).unwrap(),
        }],
        always: false,
    };
    let cfg = temp_config();
    let app = make_app_with_policy(policy, None, cfg.path().to_path_buf());

    // Enable for today
    let _ = rpc(
        &app,
        "upsert_override",
        json!({ "id": "test-game", "availability": true }),
    )
    .await;

    // At any wall-clock time, the entry should be available.
    let (status, body) = rpc(&app, "get_entry", json!({ "id": "test-game" })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enabled"], true);
}
