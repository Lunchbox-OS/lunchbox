//! Behavioral tests for the shared management surface, driven through
//! `dispatch_json` against a real `DefaultManagementService`.
//!
//! Every transport (HTTP, BLE, IPC) is a thin adapter over
//! `dispatch_json`, so the business logic and the generated dispatch
//! mechanics (method-name matching, param parsing, `wrap_result`
//! shaping, error propagation) belong here — tested once, transport-free.
//! The per-transport test suites cover only what is genuinely
//! transport-specific: HTTP status-code/bearer-auth mapping, BLE
//! framing/`ErrorCode` mapping, and so on.

use async_trait::async_trait;
use serde_json::{Value, json};
use shepherd_api::{EntryKind, Event};
use shepherd_config::{
    AvailabilityPolicy, BrightnessPolicy, Entry, LimitsPolicy, Policy, ServiceConfig, VolumePolicy,
};
use shepherd_core::CoreEngine;
use shepherd_host_api::{
    BrightnessCapabilities, BrightnessController, BrightnessResult, BrightnessStatus,
    HostCapabilities, MockHost, NoOpHidpiController, VolumeCapabilities, VolumeController,
    VolumeResult, VolumeStatus,
};
use shepherd_management::{
    DefaultManagementService, ManagementError, RpcDispatchError, dispatch_json,
};
use shepherd_store::SqliteStore;
use shepherd_util::{DaysOfWeek, EntryId, TimeWindow, WallClock};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tempfile::NamedTempFile;
use tokio::sync::{Mutex, broadcast, watch};

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
// Fixtures
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

/// Build a real `DefaultManagementService` over an in-memory store and
/// mock host/volume/brightness — the same wiring the daemon uses, minus
/// the OS-facing bits.
fn make_svc(policy: Policy, config_path: PathBuf) -> DefaultManagementService {
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
    DefaultManagementService {
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
        hidpi: Arc::new(NoOpHidpiController),
    }
}

/// Write a minimal valid config to a temp file.
fn temp_config() -> NamedTempFile {
    let f = NamedTempFile::new().unwrap();
    std::fs::write(f.path(), "config_version = 1\n").unwrap();
    f
}

/// Dispatch one method by name, exactly as every transport does.
async fn rpc(
    svc: &DefaultManagementService,
    method: &str,
    params: Value,
) -> Result<Value, RpcDispatchError> {
    dispatch_json(svc, method, params).await
}

/// Convenience: dispatch and unwrap the successful value.
async fn ok(svc: &DefaultManagementService, method: &str, params: Value) -> Value {
    rpc(svc, method, params)
        .await
        .unwrap_or_else(|e| panic!("{method} failed: {e:?}"))
}

// ---------------------------------------------------------------------------
// Dispatch mechanics (generated by `#[management_rpc]`)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unknown_method_is_method_not_found() {
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());
    let err = rpc(&svc, "no_such_method", json!({})).await.unwrap_err();
    assert!(
        matches!(err, RpcDispatchError::MethodNotFound(_)),
        "{err:?}"
    );
}

#[tokio::test]
async fn missing_required_param_is_invalid_params() {
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());
    // `upsert_override` requires an `id`, so `{}` fails param parsing.
    let err = rpc(&svc, "upsert_override", json!({})).await.unwrap_err();
    assert!(matches!(err, RpcDispatchError::InvalidParams(_)), "{err:?}");
}

#[tokio::test]
async fn zero_arg_method_accepts_object_or_null_params() {
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());
    // Zero-arg methods accept both `{}` and `null` as the params blob.
    assert!(rpc(&svc, "health", json!({})).await.is_ok());
    assert!(rpc(&svc, "health", Value::Null).await.is_ok());
}

#[tokio::test]
async fn logout_returns_null_result() {
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());
    assert_eq!(ok(&svc, "logout", Value::Null).await, Value::Null);
}

#[tokio::test]
async fn ping_returns_null_result() {
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());
    assert_eq!(ok(&svc, "ping", json!({})).await, Value::Null);
}

// ---------------------------------------------------------------------------
// Health / state
// ---------------------------------------------------------------------------

#[tokio::test]
async fn health_reports_live_and_ready() {
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());
    let body = ok(&svc, "health", json!({})).await;
    assert_eq!(body["live"], true);
    assert_eq!(body["ready"], true);
}

#[tokio::test]
async fn service_state_returns_snapshot() {
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());
    let body = ok(&svc, "service_state", json!({})).await;
    assert!(body["api_version"].is_number());
    assert!(body["current_session"].is_null());
}

// ---------------------------------------------------------------------------
// Entries
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_entries_returns_all_entries() {
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());
    let body = ok(&svc, "list_entries", json!({})).await;
    let arr = body.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["entry_id"], "test-game");
    assert_eq!(arr[0]["enabled"], true);
}

#[tokio::test]
async fn get_entry_known_returns_view() {
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());
    let body = ok(&svc, "get_entry", json!({ "id": "test-game" })).await;
    assert_eq!(body["entry_id"], "test-game");
    assert_eq!(body["label"], "Test Game");
}

#[tokio::test]
async fn get_entry_unknown_is_not_found() {
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());
    let err = rpc(&svc, "get_entry", json!({ "id": "does-not-exist" }))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            RpcDispatchError::Management(ManagementError::NotFound(_))
        ),
        "{err:?}"
    );
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn current_session_is_null_initially() {
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());
    assert!(ok(&svc, "current_session", json!({})).await.is_null());
}

#[tokio::test]
async fn launch_known_entry_is_approved() {
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());
    let body = ok(&svc, "launch", json!({ "id": "test-game" })).await;
    // LaunchOutcome is serde's externally-tagged form.
    assert!(body["Approved"].is_object());
    assert!(body["Approved"]["session_id"].is_string());
}

#[tokio::test]
async fn launch_unknown_entry_is_denied() {
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());
    let body = ok(&svc, "launch", json!({ "id": "no-such-entry" })).await;
    assert!(body["Denied"].is_object());
    assert!(body["Denied"]["reasons"].is_array());
}

#[tokio::test]
async fn stop_current_without_session_is_not_found() {
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());
    let err = rpc(&svc, "stop_current", json!({})).await.unwrap_err();
    assert!(
        matches!(
            err,
            RpcDispatchError::Management(ManagementError::NotFound(_))
        ),
        "{err:?}"
    );
}

#[tokio::test]
async fn extend_current_without_session_is_not_found() {
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());
    let err = rpc(&svc, "extend_current", json!({ "seconds": 60 }))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            RpcDispatchError::Management(ManagementError::NotFound(_))
        ),
        "{err:?}"
    );
}

#[tokio::test]
async fn sessions_full_lifecycle() {
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());

    // No session yet.
    assert!(ok(&svc, "current_session", json!({})).await.is_null());

    // Launch.
    let body = ok(&svc, "launch", json!({ "id": "test-game" })).await;
    assert!(body["Approved"].is_object());

    // Session is active.
    let body = ok(&svc, "current_session", json!({})).await;
    assert!(!body.is_null());
    assert_eq!(body["entry_id"], "test-game");

    // Second launch is denied (session already active).
    let body = ok(&svc, "launch", json!({ "id": "test-game" })).await;
    assert!(body["Denied"].is_object());

    // Stop.
    ok(&svc, "stop_current", json!({})).await;

    // Session is null again.
    assert!(ok(&svc, "current_session", json!({})).await.is_null());
}

#[tokio::test]
async fn extend_active_session_returns_new_deadline() {
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());
    ok(&svc, "launch", json!({ "id": "test-game" })).await;
    // wrap_result = "new_deadline"
    let body = ok(&svc, "extend_current", json!({ "seconds": 120 })).await;
    assert!(body["new_deadline"].is_string() || body["new_deadline"].is_null());
}

#[tokio::test]
async fn reduce_active_session_returns_new_deadline() {
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());
    ok(&svc, "launch", json!({ "id": "test-game" })).await;
    let body = ok(&svc, "extend_current", json!({ "seconds": -60 })).await;
    assert!(body["new_deadline"].is_string() || body["new_deadline"].is_null());
}

// ---------------------------------------------------------------------------
// Overrides
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_overrides_empty_initially() {
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());
    let body = ok(&svc, "list_overrides", json!({})).await;
    assert_eq!(body.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn upsert_override_creates_record() {
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());
    let body = ok(
        &svc,
        "upsert_override",
        json!({ "id": "test-game", "availability": true }),
    )
    .await;
    assert_eq!(body["entry_id"], "test-game");
    assert_eq!(body["availability"], true);
}

#[tokio::test]
async fn get_override_returns_existing_record() {
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());
    ok(
        &svc,
        "upsert_override",
        json!({ "id": "test-game", "quota_delta_seconds": 300 }),
    )
    .await;
    let body = ok(&svc, "get_override", json!({ "id": "test-game" })).await;
    assert_eq!(body["quota_delta_seconds"], 300);
}

#[tokio::test]
async fn get_override_nonexistent_is_null() {
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());
    let body = ok(&svc, "get_override", json!({ "id": "test-game" })).await;
    assert!(body.is_null());
}

#[tokio::test]
async fn upsert_override_without_fields_is_bad_request() {
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());
    // `id` present but neither `availability` nor `quota_delta_seconds`.
    let err = rpc(&svc, "upsert_override", json!({ "id": "test-game" }))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            RpcDispatchError::Management(ManagementError::BadRequest(_))
        ),
        "{err:?}"
    );
}

#[tokio::test]
async fn list_overrides_shows_created_record() {
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());
    ok(
        &svc,
        "upsert_override",
        json!({ "id": "test-game", "availability": true }),
    )
    .await;
    let body = ok(&svc, "list_overrides", json!({})).await;
    assert_eq!(body.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn delete_override_existing_wraps_true() {
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());
    ok(
        &svc,
        "upsert_override",
        json!({ "id": "test-game", "availability": true }),
    )
    .await;
    // wrap_result = "deleted"
    let body = ok(&svc, "delete_override", json!({ "id": "test-game" })).await;
    assert_eq!(body["deleted"], true);
}

#[tokio::test]
async fn delete_override_nonexistent_wraps_false() {
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());
    let body = ok(&svc, "delete_override", json!({ "id": "test-game" })).await;
    assert_eq!(body["deleted"], false);
}

// ---------------------------------------------------------------------------
// Usage
// ---------------------------------------------------------------------------

#[tokio::test]
async fn usage_all_returns_empty_without_sessions() {
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());
    assert!(ok(&svc, "usage_all", json!({})).await.is_array());
}

#[tokio::test]
async fn usage_entry_returns_empty_for_known_entry() {
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());
    assert!(
        ok(&svc, "usage_entry", json!({ "id": "test-game" }))
            .await
            .is_array()
    );
}

// ---------------------------------------------------------------------------
// Volume
// ---------------------------------------------------------------------------

#[tokio::test]
async fn get_volume_returns_status() {
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());
    let body = ok(&svc, "get_volume", json!({})).await;
    assert_eq!(body["percent"], 50);
    assert_eq!(body["muted"], false);
}

#[tokio::test]
async fn set_volume_updates_percent() {
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());
    let body = ok(&svc, "set_volume", json!({ "percent": 75 })).await;
    assert_eq!(body["percent"], 75);
}

#[tokio::test]
async fn set_mute_updates_muted() {
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());
    let body = ok(&svc, "set_mute", json!({ "muted": true })).await;
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
    let svc = make_svc(policy, cfg.path().to_path_buf());
    let body = ok(&svc, "set_volume", json!({ "percent": 95 })).await;
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
    let svc = make_svc(policy, cfg.path().to_path_buf());
    let err = rpc(&svc, "set_volume", json!({ "percent": 75 }))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            RpcDispatchError::Management(ManagementError::Forbidden(_))
        ),
        "{err:?}"
    );
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
    let svc = make_svc(policy, cfg.path().to_path_buf());
    let err = rpc(&svc, "set_mute", json!({ "muted": true }))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            RpcDispatchError::Management(ManagementError::Forbidden(_))
        ),
        "{err:?}"
    );
}

#[tokio::test]
async fn volume_up_and_down_walk_through_step() {
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());
    let body = ok(&svc, "volume_up", json!({ "step": 10 })).await;
    assert_eq!(body["percent"], 60);
    let body = ok(&svc, "volume_down", json!({ "step": 25 })).await;
    assert_eq!(body["percent"], 35);
}

#[tokio::test]
async fn toggle_mute_flips_state() {
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());
    let body = ok(&svc, "toggle_mute", json!({})).await;
    assert_eq!(body["muted"], true);
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reload_config_valid_file_wraps_entry_count() {
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());
    // wrap_result = "entry_count"
    let body = ok(&svc, "reload_config", json!({})).await;
    assert!(body["entry_count"].is_number());
}

#[tokio::test]
async fn reload_config_invalid_file_is_unprocessable() {
    let cfg = NamedTempFile::new().unwrap();
    std::fs::write(cfg.path(), "not = valid = toml").unwrap();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());
    let err = rpc(&svc, "reload_config", json!({})).await.unwrap_err();
    assert!(
        matches!(
            err,
            RpcDispatchError::Management(ManagementError::Unprocessable(_))
        ),
        "{err:?}"
    );
}

// ---------------------------------------------------------------------------
// Override / policy interactions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn disabled_entry_via_override_shows_as_unavailable() {
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());

    // Disable for today via override.
    ok(
        &svc,
        "upsert_override",
        json!({ "id": "test-game", "availability": false }),
    )
    .await;

    let body = ok(&svc, "get_entry", json!({ "id": "test-game" })).await;
    assert_eq!(body["enabled"], false);
}

#[tokio::test]
async fn enable_entry_outside_time_window_via_override() {
    // Entry with a lunch-only window that we enable "today" via
    // override — the override should override the time window at the
    // policy level so `get_entry` reports it available at any time.
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
    let svc = make_svc(policy, cfg.path().to_path_buf());

    ok(
        &svc,
        "upsert_override",
        json!({ "id": "test-game", "availability": true }),
    )
    .await;

    let body = ok(&svc, "get_entry", json!({ "id": "test-game" })).await;
    assert_eq!(body["enabled"], true);
}
