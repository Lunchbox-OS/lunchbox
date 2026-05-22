//! Integration tests for the shepherd-http management API.

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
            input_compat: vec![],
            input_compat_options: Default::default(),
            xwayland_native_resolution: false,
        }],
        default_warnings: vec![],
        default_max_run: Some(Duration::from_secs(3600)),
        volume: VolumePolicy::default(),
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
    let state = AppState {
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
    };
    handlers::router(state, auth_token.map(str::to_owned))
}

fn make_app(auth_token: Option<&str>, config_path: PathBuf) -> axum::Router {
    make_app_with_policy(test_policy(), auth_token, config_path)
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

fn req_get(uri: &str) -> Request<Body> {
    Request::builder().uri(uri).body(Body::empty()).unwrap()
}

fn req_get_auth(uri: &str, token: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

fn req_post_json(uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn req_put_json(uri: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method("PUT")
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

fn req_post(uri: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

fn req_delete(uri: &str) -> Request<Body> {
    Request::builder()
        .method("DELETE")
        .uri(uri)
        .body(Body::empty())
        .unwrap()
}

// ---------------------------------------------------------------------------
// Health
// ---------------------------------------------------------------------------

#[tokio::test]
async fn health_returns_ok() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = send(&app, req_get("/api/v1/health")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["live"], true);
    assert_eq!(body["ready"], true);
}

#[tokio::test]
async fn state_returns_snapshot() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = send(&app, req_get("/api/v1/state")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["api_version"].is_number());
    assert!(body["active_session"].is_null());
}

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------

#[tokio::test]
async fn auth_no_token_configured_allows_all() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, _) = send(&app, req_get("/api/v1/health")).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn auth_missing_header_returns_401() {
    let cfg = temp_config();
    let app = make_app(Some("secret"), cfg.path().to_path_buf());
    let (status, body) = send(&app, req_get("/api/v1/health")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "unauthorized");
}

#[tokio::test]
async fn auth_wrong_token_returns_401() {
    let cfg = temp_config();
    let app = make_app(Some("secret"), cfg.path().to_path_buf());
    let (status, _) = send(&app, req_get_auth("/api/v1/health", "wrong")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn auth_correct_token_passes() {
    let cfg = temp_config();
    let app = make_app(Some("secret"), cfg.path().to_path_buf());
    let (status, _) = send(&app, req_get_auth("/api/v1/health", "secret")).await;
    assert_eq!(status, StatusCode::OK);
}

// ---------------------------------------------------------------------------
// Entries
// ---------------------------------------------------------------------------

#[tokio::test]
async fn entries_list_returns_all_entries() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = send(&app, req_get("/api/v1/entries")).await;
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
    let (status, body) = send(&app, req_get("/api/v1/entries/test-game")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["entry_id"], "test-game");
    assert_eq!(body["label"], "Test Game");
}

#[tokio::test]
async fn entry_get_unknown_returns_404() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = send(&app, req_get("/api/v1/entries/does-not-exist")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "not_found");
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sessions_current_is_null_initially() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = send(&app, req_get("/api/v1/sessions/current")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_null());
}

#[tokio::test]
async fn sessions_launch_known_entry_is_approved() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = send(
        &app,
        req_post_json("/api/v1/sessions", json!({ "entry_id": "test-game" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["result"], "approved");
    assert!(body["session_id"].is_string());
}

#[tokio::test]
async fn sessions_launch_unknown_entry_is_denied() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = send(
        &app,
        req_post_json("/api/v1/sessions", json!({ "entry_id": "no-such-entry" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["result"], "denied");
}

#[tokio::test]
async fn sessions_stop_no_active_session_returns_404() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = send(&app, req_delete("/api/v1/sessions/current")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "no_active_session");
}

#[tokio::test]
async fn sessions_extend_no_active_session_returns_404() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, _) = send(
        &app,
        req_post_json("/api/v1/sessions/current/extend", json!({ "seconds": 60 })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn sessions_full_lifecycle() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());

    // No session yet
    let (status, body) = send(&app, req_get("/api/v1/sessions/current")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_null());

    // Launch
    let (status, body) = send(
        &app,
        req_post_json("/api/v1/sessions", json!({ "entry_id": "test-game" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["result"], "approved");

    // Session is active
    let (status, body) = send(&app, req_get("/api/v1/sessions/current")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.is_null());
    assert_eq!(body["entry_id"], "test-game");

    // Second launch is denied (session already active)
    let (status, body) = send(
        &app,
        req_post_json("/api/v1/sessions", json!({ "entry_id": "test-game" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["result"], "denied");

    // Stop
    let (status, _) = send(&app, req_delete("/api/v1/sessions/current")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Session gone
    let (status, body) = send(&app, req_get("/api/v1/sessions/current")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_null());
}

#[tokio::test]
async fn sessions_extend_active_session() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());

    // Launch
    let (status, _) = send(
        &app,
        req_post_json("/api/v1/sessions", json!({ "entry_id": "test-game" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Extend
    let (status, body) = send(
        &app,
        req_post_json("/api/v1/sessions/current/extend", json!({ "seconds": 60 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // Entry has max_run=300s so there is a deadline
    assert!(!body["new_deadline"].is_null());
}

#[tokio::test]
async fn sessions_reduce_active_session() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());

    // Launch
    let (status, _) = send(
        &app,
        req_post_json("/api/v1/sessions", json!({ "entry_id": "test-game" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Reduce time (negative seconds)
    let (status, body) = send(
        &app,
        req_post_json("/api/v1/sessions/current/extend", json!({ "seconds": -30 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body["new_deadline"].is_null());
}

// ---------------------------------------------------------------------------
// Daily overrides
// ---------------------------------------------------------------------------

#[tokio::test]
async fn overrides_list_empty_initially() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = send(&app, req_get("/api/v1/overrides")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn overrides_upsert_creates_record() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = send(
        &app,
        req_put_json(
            "/api/v1/overrides/test-game",
            json!({ "availability": false }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["entry_id"], "test-game");
    assert_eq!(body["availability"], false);
}

#[tokio::test]
async fn overrides_get_returns_existing_record() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());

    // Create
    let (status, _) = send(
        &app,
        req_put_json(
            "/api/v1/overrides/test-game",
            json!({ "quota_delta_seconds": 1800 }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Retrieve
    let (status, body) = send(&app, req_get("/api/v1/overrides/test-game")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["entry_id"], "test-game");
    assert_eq!(body["quota_delta_seconds"], 1800);
}

#[tokio::test]
async fn overrides_get_nonexistent_returns_null() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = send(&app, req_get("/api/v1/overrides/test-game")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_null());
}

#[tokio::test]
async fn overrides_upsert_empty_body_returns_400() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = send(&app, req_put_json("/api/v1/overrides/test-game", json!({}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "bad_request");
}

#[tokio::test]
async fn overrides_list_shows_created_record() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());

    // Create
    let (status, _) = send(
        &app,
        req_put_json(
            "/api/v1/overrides/test-game",
            json!({ "availability": true }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // List
    let (status, body) = send(&app, req_get("/api/v1/overrides")).await;
    assert_eq!(status, StatusCode::OK);
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["entry_id"], "test-game");
}

#[tokio::test]
async fn overrides_delete_existing_returns_204() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());

    // Create
    let (status, _) = send(
        &app,
        req_put_json(
            "/api/v1/overrides/test-game",
            json!({ "availability": false }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Delete
    let (status, _) = send(&app, req_delete("/api/v1/overrides/test-game")).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    // Gone
    let (status, body) = send(&app, req_get("/api/v1/overrides/test-game")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_null());
}

#[tokio::test]
async fn overrides_delete_nonexistent_returns_404() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, _) = send(&app, req_delete("/api/v1/overrides/test-game")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn overrides_disabled_entry_shows_as_unavailable() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());

    // Entry is enabled before override
    let (status, body) = send(&app, req_get("/api/v1/entries/test-game")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enabled"], true);

    // Disable via override
    let (status, _) = send(
        &app,
        req_put_json(
            "/api/v1/overrides/test-game",
            json!({ "availability": false }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Entry is now disabled
    let (status, body) = send(&app, req_get("/api/v1/entries/test-game")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enabled"], false);
    // ReasonCode serializes as { "code": "manually_disabled", ... }
    let reasons = body["reasons"].as_array().unwrap();
    assert!(reasons.iter().any(|r| r["code"] == "manually_disabled"));
}

#[tokio::test]
async fn overrides_enable_entry_outside_time_window() {
    // Build a policy with a narrow time window (23:55–23:59) so the entry is
    // almost always outside its allowed hours.
    let windowed_policy = Policy {
        service: ServiceConfig::default(),
        entries: vec![Entry {
            id: EntryId::new("time-restricted"),
            label: "Time Restricted".into(),
            icon_ref: None,
            kind: EntryKind::Process {
                command: "game".into(),
                args: vec![],
                env: HashMap::new(),
                cwd: None,
            },
            availability: AvailabilityPolicy {
                windows: vec![TimeWindow::new(
                    DaysOfWeek::ALL_DAYS,
                    WallClock::new(23, 55).unwrap(),
                    WallClock::new(23, 59).unwrap(),
                )],
                always: false,
            },
            limits: LimitsPolicy {
                max_run: None,
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
            input_compat: vec![],
            input_compat_options: Default::default(),
            xwayland_native_resolution: false,
        }],
        default_warnings: vec![],
        default_max_run: None,
        volume: VolumePolicy::default(),
        brightness: BrightnessPolicy::default(),
    };

    let cfg = temp_config();
    let app = make_app_with_policy(windowed_policy, None, cfg.path().to_path_buf());

    // Entry should be disabled (outside the 23:55–23:59 window) at current time.
    // We verify the API reflects the right state without relying on a fixed clock.
    let (status, body) = send(&app, req_get("/api/v1/entries/time-restricted")).await;
    assert_eq!(status, StatusCode::OK);
    // Entry is disabled — outside the window
    assert!(!body["enabled"].as_bool().unwrap());
    let reasons = body["reasons"].as_array().unwrap();
    assert!(
        reasons.iter().any(|r| r["code"] == "outside_time_window"),
        "expected outside_time_window, got: {reasons:?}"
    );

    // Enable via override (no date = today)
    let (status, _) = send(
        &app,
        req_put_json(
            "/api/v1/overrides/time-restricted",
            json!({ "availability": true }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // Entry should now be enabled despite being outside the window
    let (status, body) = send(&app, req_get("/api/v1/entries/time-restricted")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["enabled"].as_bool().unwrap(),
        "entry should be enabled after override, got: {body}"
    );
    assert!(
        body["reasons"].as_array().unwrap().is_empty(),
        "no reasons expected when enabled: {}",
        body["reasons"]
    );

    // Launch should also be approved
    let (status, body) = send(
        &app,
        req_post_json("/api/v1/sessions", json!({ "entry_id": "time-restricted" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["result"], "approved");
}

// ---------------------------------------------------------------------------
// Usage
// ---------------------------------------------------------------------------

#[tokio::test]
async fn usage_all_returns_empty_without_any_sessions() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = send(&app, req_get("/api/v1/usage")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn usage_entry_returns_empty_for_known_entry() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = send(&app, req_get("/api/v1/usage/test-game")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn usage_entry_unknown_returns_404() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = send(&app, req_get("/api/v1/usage/no-such-entry")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "not_found");
}

#[tokio::test]
async fn usage_invalid_date_range_returns_400() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    // from is after to
    let (status, body) = send(&app, req_get("/api/v1/usage?from=2026-04-25&to=2026-04-01")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "bad_request");
}

#[tokio::test]
async fn usage_date_range_returns_stats_in_range() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    // Valid range with no data — just check it doesn't error
    let (status, body) = send(&app, req_get("/api/v1/usage?from=2026-01-01&to=2026-04-25")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.is_array());
}

// ---------------------------------------------------------------------------
// Volume
// ---------------------------------------------------------------------------

#[tokio::test]
async fn volume_get_returns_status() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = send(&app, req_get("/api/v1/volume")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["percent"].is_number());
    assert!(body["muted"].is_boolean());
    assert_eq!(body["available"], true);
}

#[tokio::test]
async fn volume_set_percent_updates_volume() {
    let cfg = temp_config();
    let app = make_app_with_policy(
        Policy {
            volume: VolumePolicy::unrestricted(),
            ..test_policy()
        },
        None,
        cfg.path().to_path_buf(),
    );
    let (status, body) = send(
        &app,
        req_put_json("/api/v1/volume", json!({ "percent": 70 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["percent"], 70);
}

#[tokio::test]
async fn volume_set_muted_updates_mute_state() {
    let cfg = temp_config();
    let app = make_app_with_policy(
        Policy {
            volume: VolumePolicy::unrestricted(),
            ..test_policy()
        },
        None,
        cfg.path().to_path_buf(),
    );
    let (status, body) = send(
        &app,
        req_put_json("/api/v1/volume", json!({ "muted": true })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["muted"], true);
}

#[tokio::test]
async fn volume_set_above_max_is_clamped() {
    let cfg = temp_config();
    let app = make_app_with_policy(
        Policy {
            volume: VolumePolicy {
                max_volume: Some(80),
                allow_change: true,
                allow_mute: true,
                ..Default::default()
            },
            ..test_policy()
        },
        None,
        cfg.path().to_path_buf(),
    );
    // Request 95%, expect it clamped to 80
    let (status, body) = send(
        &app,
        req_put_json("/api/v1/volume", json!({ "percent": 95 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["percent"], 80);
}

#[tokio::test]
async fn volume_set_forbidden_when_change_disallowed() {
    let cfg = temp_config();
    let app = make_app_with_policy(
        Policy {
            volume: VolumePolicy {
                allow_change: false,
                allow_mute: false,
                ..Default::default()
            },
            ..test_policy()
        },
        None,
        cfg.path().to_path_buf(),
    );
    let (status, body) = send(
        &app,
        req_put_json("/api/v1/volume", json!({ "percent": 50 })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "forbidden");
}

#[tokio::test]
async fn volume_mute_forbidden_when_mute_disallowed() {
    let cfg = temp_config();
    let app = make_app_with_policy(
        Policy {
            volume: VolumePolicy {
                allow_change: true,
                allow_mute: false,
                ..Default::default()
            },
            ..test_policy()
        },
        None,
        cfg.path().to_path_buf(),
    );
    let (status, body) = send(
        &app,
        req_put_json("/api/v1/volume", json!({ "muted": true })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["error"], "forbidden");
}

// ---------------------------------------------------------------------------
// Config reload
// ---------------------------------------------------------------------------

#[tokio::test]
async fn config_reload_valid_file_returns_200() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = send(&app, req_post("/api/v1/config/reload")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["entry_count"].is_number());
}

#[tokio::test]
async fn config_reload_invalid_file_returns_422() {
    let f = NamedTempFile::new().unwrap();
    std::fs::write(f.path(), "this is not valid toml !!!").unwrap();
    let app = make_app(None, f.path().to_path_buf());
    let (status, body) = send(&app, req_post("/api/v1/config/reload")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "config_error");
}

// ---------------------------------------------------------------------------
// Debug
// ---------------------------------------------------------------------------

#[tokio::test]
async fn debug_windows_unsupported_returns_500() {
    // MockHost does not implement list_windows, so the default trait impl
    // returns HostError::Internal — surfaced as a 500 with a JSON error body.
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    let (status, body) = send(&app, req_get("/api/v1/debug/windows")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["error"], "internal_error");
}

#[tokio::test]
async fn debug_window_actions_unsupported_return_500() {
    let cfg = temp_config();
    let app = make_app(None, cfg.path().to_path_buf());
    for path in ["close", "hide", "show"] {
        let uri = format!("/api/v1/debug/windows/42/{path}");
        let (status, body) = send(&app, req_post(&uri)).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{path}");
        assert_eq!(body["error"], "internal_error", "{path}");
    }
}

#[tokio::test]
async fn config_reload_updates_policy() {
    // Start with a policy with one entry, reload with zero entries
    let f = NamedTempFile::new().unwrap();
    std::fs::write(f.path(), "config_version = 1\n").unwrap();
    let cfg_path = f.path().to_path_buf();

    let app = make_app(None, cfg_path.clone());

    // Before reload: one entry
    let (_, body) = send(&app, req_get("/api/v1/entries")).await;
    assert_eq!(body.as_array().unwrap().len(), 1);

    // Reload with config that has no entries
    let (status, body) = send(&app, req_post("/api/v1/config/reload")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["entry_count"], 0);

    // After reload: zero entries
    let (_, body) = send(&app, req_get("/api/v1/entries")).await;
    assert_eq!(body.as_array().unwrap().len(), 0);
}
