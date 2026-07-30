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
use shepherd_api::{
    AddressFamily, Connectivity, EntryKind, Event, EventPayload, NetworkAddressView,
    NetworkInterfaceKind, NetworkInterfaceView, NetworkSource, WifiView,
};
use shepherd_config::{
    AutoBrightnessPolicy, AvailabilityPolicy, BrightnessPolicy, Entry, LimitsPolicy, Policy,
    ServiceConfig, TokensPolicy, VolumePolicy,
};
use shepherd_core::CoreEngine;
use shepherd_host_api::{
    BrightnessCapabilities, BrightnessController, BrightnessResult, BrightnessStatus,
    HostCapabilities, LightSensor, LightSensorCapabilities, LightSensorResult, MockHost,
    NetworkSnapshot, NoOpDisplayController, NoOpHidpiController, NoOpHudLayoutController,
    StaticNetworkInfo, VolumeCapabilities, VolumeController, VolumeResult, VolumeStatus,
};
use shepherd_management::{
    AUTO_BRIGHTNESS_SETTING_KEY, AutoBrightnessState, DefaultManagementService, ManagementError,
    ManagementService, RpcDispatchError, WebListenerHandle, dispatch_json,
};
use shepherd_store::SqliteStore;
use shepherd_util::{
    DaysOfWeek, EntryId, LimitSubject, LocalProtectedFiles, TimeWindow, WallClock,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tempfile::{NamedTempFile, TempDir};
use tokio::sync::{Mutex, broadcast, watch};

// ---------------------------------------------------------------------------
// MockVolume
// ---------------------------------------------------------------------------

struct MockVolume {
    capabilities: VolumeCapabilities,
    status: std::sync::Mutex<VolumeStatus>,
    /// The active output, swappable so tests can simulate a sink switch.
    output: std::sync::Mutex<Option<shepherd_api::AudioOutput>>,
    /// Outputs that are plugged in right now: what `list_outputs` reports, and
    /// the only things `select_output` will move to. Distinct from the rows the
    /// store remembers, which outlive the hardware.
    present: std::sync::Mutex<Vec<shepherd_api::AudioOutput>>,
    /// When false, reads fail the way a `pw-dump` that will not run fails.
    /// Distinct from having no devices — which is the whole point.
    readable: std::sync::Mutex<bool>,
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
            output: std::sync::Mutex::new(None),
            present: std::sync::Mutex::new(Vec::new()),
            readable: std::sync::Mutex::new(true),
        }
    }

    /// Make topology reads fail, leaving the devices themselves untouched.
    fn break_topology(&self) {
        *self.readable.lock().unwrap() = false;
    }

    fn fix_topology(&self) {
        *self.readable.lock().unwrap() = true;
    }

    /// Connect a device without selecting it.
    fn plug_in(&self, o: shepherd_api::AudioOutput) {
        let mut present = self.present.lock().unwrap();
        if !present.iter().any(|p| p.key == o.key) {
            present.push(o);
        }
    }

    fn unplug(&self, key: &str) {
        self.present.lock().unwrap().retain(|p| p.key != key);
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

    async fn current_output(&self) -> Option<shepherd_api::AudioOutput> {
        if !*self.readable.lock().unwrap() {
            return None;
        }
        self.output.lock().unwrap().clone()
    }

    /// The mock's whole point: one read that reports the reading, what is
    /// selected, and everything plugged in — the shape a real backend gets from
    /// a single `pw-dump`.
    async fn observe(&self) -> VolumeResult<shepherd_host_api::AudioSnapshot> {
        if !*self.readable.lock().unwrap() {
            return Err(shepherd_host_api::VolumeError::Backend(
                "could not read the audio topology".into(),
            ));
        }
        Ok(shepherd_host_api::AudioSnapshot {
            status: self.status.lock().unwrap().clone(),
            active: self.output.lock().unwrap().clone(),
            outputs: self.present.lock().unwrap().clone(),
        })
    }

    async fn select_output(&self, output_key: &str) -> VolumeResult<()> {
        let found = self
            .present
            .lock()
            .unwrap()
            .iter()
            .find(|o| o.key == output_key)
            .cloned();
        match found {
            Some(o) => {
                *self.output.lock().unwrap() = Some(o);
                Ok(())
            }
            None => Err(shepherd_host_api::VolumeError::NotAvailable(format!(
                "not connected: {output_key}"
            ))),
        }
    }
}

fn output(key: &str, description: &str) -> shepherd_api::AudioOutput {
    shepherd_api::AudioOutput {
        key: key.into(),
        description: description.into(),
        kind: shepherd_api::AudioOutputKind::Unknown,
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
// MockLightSensor
// ---------------------------------------------------------------------------

struct MockLightSensor {
    capabilities: LightSensorCapabilities,
    lux: std::sync::Mutex<f32>,
}

impl MockLightSensor {
    fn new(lux: f32) -> Self {
        Self {
            capabilities: LightSensorCapabilities {
                available: true,
                device: Some("mock-als".into()),
            },
            lux: std::sync::Mutex::new(lux),
        }
    }
}

impl LightSensor for MockLightSensor {
    fn capabilities(&self) -> &LightSensorCapabilities {
        &self.capabilities
    }
    fn read_lux(&self) -> LightSensorResult<f32> {
        Ok(*self.lux.lock().unwrap())
    }
}

// ---------------------------------------------------------------------------
// Fixtures
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

/// Build a real `DefaultManagementService` over an in-memory store and
/// mock host/volume/brightness — the same wiring the daemon uses, minus
/// the OS-facing bits. Includes a mock ambient light sensor (bright room).
fn make_svc(policy: Policy, config_path: PathBuf) -> DefaultManagementService {
    make_svc_opts(policy, config_path, Some(1000.0)).0
}

/// Like [`make_svc`], but also hands back the `MockHost` so a test can inject
/// host failures (e.g. `fail_spawn`).
fn make_svc_with_host(
    policy: Policy,
    config_path: PathBuf,
) -> (DefaultManagementService, Arc<MockHost>) {
    make_svc_opts(policy, config_path, Some(1000.0))
}

/// Like [`make_svc`], but `sensor_lux` controls the ambient light sensor:
/// `Some(lux)` installs a mock sensor reading that value, `None` models a host
/// with no light sensor (auto brightness unavailable).
fn make_svc_opts(
    policy: Policy,
    config_path: PathBuf,
    sensor_lux: Option<f32>,
) -> (DefaultManagementService, Arc<MockHost>) {
    make_svc_full(policy, config_path, sensor_lux, Arc::new(MockVolume::new()))
}

/// Like [`make_svc`], but hands back the volume mock so a test can drive the
/// host-side state the service only observes.
fn make_svc_with_volume(
    policy: Policy,
    config_path: PathBuf,
) -> (DefaultManagementService, Arc<MockVolume>) {
    let volume = Arc::new(MockVolume::new());
    let svc = make_svc_full(policy, config_path, Some(1000.0), volume.clone()).0;
    (svc, volume)
}

fn make_svc_full(
    policy: Policy,
    config_path: PathBuf,
    sensor_lux: Option<f32>,
    volume: Arc<MockVolume>,
) -> (DefaultManagementService, Arc<MockHost>) {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let host = Arc::new(MockHost::new());
    let brightness = Arc::new(MockBrightness::new());
    let light_sensor: Option<Arc<dyn LightSensor>> =
        sensor_lux.map(|lux| Arc::new(MockLightSensor::new(lux)) as Arc<dyn LightSensor>);
    let engine = Arc::new(Mutex::new(CoreEngine::new(
        policy,
        store.clone(),
        HostCapabilities::minimal(),
    )));
    let (tx, _) = broadcast::channel::<Event>(64);
    let tx_for_fn = tx.clone();
    let (shutdown_tx, _shutdown_rx) = watch::channel(false);
    let svc = DefaultManagementService {
        engine,
        store,
        host: host.clone(),
        volume,
        brightness,
        light_sensor,
        auto_brightness: Arc::new(Mutex::new(AutoBrightnessState::new(false))),
        event_tx: tx,
        broadcast_fn: Arc::new(move |event: Event| {
            let _ = tx_for_fn.send(event);
        }),
        config_path,
        policy_files: None,
        media_refresh_tx: None,
        shutdown_tx,
        hidpi: Arc::new(NoOpHidpiController),
        hud_layout: Arc::new(NoOpHudLayoutController),
        display: Arc::new(NoOpDisplayController),
        last_audio_state: Arc::new(Mutex::new(None)),
        diagnostics: None,
        // The dispatch tests that care about networking build their own
        // provider; the rest get a host that cannot look, which is a real
        // shape a device can be in and must not panic.
        network: None,
        web_listener: WebListenerHandle::default(),
        web_auth: None,
        admins: Default::default(),
    };
    (svc, host)
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

/// Await the next broadcast event's payload, failing the test rather than
/// hanging if none arrives.
async fn next_payload(rx: &mut broadcast::Receiver<Event>) -> EventPayload {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for an event")
        .expect("event channel closed")
        .payload
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

/// A successful launch announces the session exactly once. The pre-spawn
/// broadcast *replaced* the old post-spawn one rather than adding to it, so a
/// regression here would show up as two `SessionStarted` for one launch.
#[tokio::test]
async fn successful_launch_announces_the_session_once() {
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());
    let mut events = svc.subscribe_events();

    ok(&svc, "launch", json!({ "id": "test-game" })).await;

    let first = next_payload(&mut events).await;
    assert!(
        matches!(first, EventPayload::SessionStarted { .. }),
        "expected SessionStarted, got {first:?}"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(200), events.recv())
            .await
            .is_err(),
        "a successful launch should broadcast SessionStarted once and nothing further"
    );
}

/// `launch` announces the session *before* spawning, so the HUD can draw (and
/// stop) a launch that takes a long time to present a window — an Android cold
/// start can hold `host.spawn` for tens of seconds.
///
/// The observable consequence is this: even a spawn that ultimately *fails* is
/// preceded by `SessionStarted`, which must then be retracted with a matching
/// `SessionEnded`. Previously a failed spawn emitted neither, so this pins both
/// halves of the new contract.
#[tokio::test]
async fn failed_spawn_retracts_the_announced_session() {
    let cfg = temp_config();
    let (svc, host) = make_svc_with_host(test_policy(), cfg.path().to_path_buf());
    *host.fail_spawn.lock().unwrap() = true;
    let mut events = svc.subscribe_events();

    let err = rpc(&svc, "launch", json!({ "id": "test-game" }))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            RpcDispatchError::Management(ManagementError::Internal(_))
        ),
        "{err:?}"
    );

    let started = next_payload(&mut events).await;
    let EventPayload::SessionStarted { session_id, .. } = started else {
        panic!("a failed spawn should still be preceded by SessionStarted, got {started:?}");
    };
    let ended = next_payload(&mut events).await;
    let EventPayload::SessionEnded {
        session_id: ended_id,
        ..
    } = ended
    else {
        panic!("expected SessionEnded to retract the announced session, got {ended:?}");
    };
    assert_eq!(
        session_id, ended_id,
        "the retraction must name the session that was announced"
    );

    // ...and the engine is left with nothing running.
    assert!(ok(&svc, "current_session", json!({})).await.is_null());
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
    assert_eq!(body["subject"], "test-game");
    assert_eq!(body["availability"], true);
}

#[tokio::test]
async fn upsert_override_accepts_a_group_subject() {
    // A caregiver can switch a whole category off for the day with one call
    // (issue #5); `group:` marks the id as a group rather than an entry.
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());
    let body = ok(
        &svc,
        "upsert_override",
        json!({ "id": "group:games", "availability": false }),
    )
    .await;
    assert_eq!(body["subject"], "group:games");
    assert_eq!(body["availability"], false);

    // It round-trips as a group, distinct from an entry of the same name.
    let body = ok(&svc, "get_override", json!({ "id": "group:games" })).await;
    assert_eq!(body["subject"], "group:games");
    let body = ok(&svc, "get_override", json!({ "id": "games" })).await;
    assert!(
        body.is_null(),
        "an entry id must not match a group override"
    );
}

/// The manual-grant wire contract (issue #8): `adjust_tokens` takes a limit
/// subject and a signed delta, and reports the gate's state back.
#[tokio::test]
async fn adjust_tokens_grants_banked_time_and_reports_the_gate() {
    let cfg = temp_config();
    let mut policy = test_policy();
    policy.entries[0].tokens = Some(TokensPolicy {
        from: vec![LimitSubject::entry("other")],
        earn_ratio: 1.0,
        minimum: Duration::from_secs(600),
        max_balance: None,
        carry_over: false,
    });
    let svc = make_svc(policy, cfg.path().to_path_buf());

    let body = ok(
        &svc,
        "adjust_tokens",
        json!({ "id": "test-game", "delta_seconds": 300 }),
    )
    .await;
    assert_eq!(body["balance"]["secs"], 300);
    assert_eq!(body["minimum"]["secs"], 600);
    assert_eq!(body["unlocked"], false);

    // Crossing the minimum opens the gate; the balance is cumulative.
    let body = ok(
        &svc,
        "adjust_tokens",
        json!({ "id": "test-game", "delta_seconds": 300 }),
    )
    .await;
    assert_eq!(body["balance"]["secs"], 600);
    assert_eq!(body["unlocked"], true);
}

#[tokio::test]
async fn adjust_tokens_rejects_an_ungated_subject() {
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());

    // `test-game` exists but has no [tokens] block, so a balance on it would
    // be written and never read.
    let err = rpc(
        &svc,
        "adjust_tokens",
        json!({ "id": "test-game", "delta_seconds": 300 }),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            err,
            RpcDispatchError::Management(ManagementError::Unprocessable(_))
        ),
        "expected Unprocessable, got {err:?}"
    );

    let err = rpc(
        &svc,
        "adjust_tokens",
        json!({ "id": "group:nope", "delta_seconds": 300 }),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            err,
            RpcDispatchError::Management(ManagementError::NotFound(_))
        ),
        "expected NotFound, got {err:?}"
    );
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
async fn set_auto_brightness_enables_persists_and_applies() {
    let cfg = temp_config();
    // Bright room (1000 lux) → default curve drives brightness to max.
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());
    let body = ok(&svc, "set_auto_brightness", json!({ "enabled": true })).await;
    assert_eq!(body["auto_available"], true);
    assert_eq!(body["auto_enabled"], true);
    // Enabling applies immediately: the mock backlight (started at 50%) is
    // driven to the bright end of the curve.
    assert_eq!(body["percent"], 100);
    // The choice is persisted so it survives a restart.
    assert_eq!(
        svc.store.get_setting(AUTO_BRIGHTNESS_SETTING_KEY).unwrap(),
        Some("true".to_string())
    );
}

#[tokio::test]
async fn toggle_auto_brightness_flips_state() {
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());
    let on = ok(&svc, "toggle_auto_brightness", Value::Null).await;
    assert_eq!(on["auto_enabled"], true);
    let off = ok(&svc, "toggle_auto_brightness", Value::Null).await;
    assert_eq!(off["auto_enabled"], false);
    assert_eq!(
        svc.store.get_setting(AUTO_BRIGHTNESS_SETTING_KEY).unwrap(),
        Some("false".to_string())
    );
}

#[tokio::test]
async fn set_auto_brightness_without_sensor_is_rejected() {
    let cfg = temp_config();
    let (svc, _host) = make_svc_opts(test_policy(), cfg.path().to_path_buf(), None);
    // No sensor → get_brightness reports auto unavailable.
    let info = ok(&svc, "get_brightness", Value::Null).await;
    assert_eq!(info["auto_available"], false);
    // …and enabling is refused.
    let err = rpc(&svc, "set_auto_brightness", json!({ "enabled": true }))
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            RpcDispatchError::Management(ManagementError::Unprocessable(_))
        ),
        "{err:?}"
    );
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

/// A policy with `n` entries, as TOML — the shape an editor would send.
fn policy_toml(ids: &[&str]) -> String {
    let mut out = String::from("config_version = 1\n");
    for id in ids {
        out.push_str(&format!(
            "\n[[entries]]\nid = \"{id}\"\nlabel = \"{id}\"\n\n[entries.kind]\ntype = \"process\"\ncommand = \"/bin/true\"\n"
        ));
    }
    out
}

/// A service whose policy the custodian holds, plus the temp dir standing in
/// for `/var/lib/shepherdd/state/<user>/`.
///
/// `LocalProtectedFiles` is the real implementation the custodian itself uses,
/// so this exercises the same code path a device does — only the directory
/// differs.
fn make_custodial_svc(local_signpost: &Path) -> (DefaultManagementService, TempDir) {
    let dir = TempDir::new().unwrap();
    std::fs::write(dir.path().join("config.toml"), policy_toml(&["a", "b"])).unwrap();
    let mut svc = make_svc(test_policy(), local_signpost.to_path_buf());
    svc.policy_files = Some(Arc::new(LocalProtectedFiles::new(dir.path().to_path_buf())));
    (svc, dir)
}

/// The signpost `shepherd install state` leaves behind: a valid policy that
/// grants nothing, at the path the policy used to live at.
fn temp_signpost() -> NamedTempFile {
    let f = NamedTempFile::new().unwrap();
    std::fs::write(f.path(), "config_version = 1\n").unwrap();
    f
}

#[tokio::test]
async fn reload_config_reads_the_custodian_not_the_signpost() {
    // The bug this covers: reloading from `config_path` on a device with a
    // custodian reloaded the zero-entry signpost, which empties the launcher.
    let signpost = temp_signpost();
    let (svc, _dir) = make_custodial_svc(signpost.path());
    let body = ok(&svc, "reload_config", json!({})).await;
    assert_eq!(body["entry_count"], 2);
}

#[tokio::test]
async fn read_policy_hands_back_the_custodians_bytes() {
    let signpost = temp_signpost();
    let (svc, _dir) = make_custodial_svc(signpost.path());
    let doc = svc.read_policy().unwrap();
    assert!(doc.text.contains("id = \"a\""), "{}", doc.text);
    assert!(!doc.version.is_empty());
}

#[tokio::test]
async fn read_policy_hands_back_a_config_that_does_not_parse() {
    // The case an editor is most needed for. Refusing here would leave the
    // only fix to a device that may have no shell on it.
    let signpost = temp_signpost();
    let (svc, dir) = make_custodial_svc(signpost.path());
    std::fs::write(dir.path().join("config.toml"), "not = valid = toml").unwrap();
    assert_eq!(svc.read_policy().unwrap().text, "not = valid = toml");
}

#[tokio::test]
async fn write_policy_replaces_the_file_and_reload_sees_it() {
    let signpost = temp_signpost();
    let (svc, dir) = make_custodial_svc(signpost.path());
    let version = svc.read_policy().unwrap().version;

    let written = svc
        .write_policy(&policy_toml(&["a", "b", "c"]), Some(&version))
        .unwrap();
    assert_ne!(written.version, version);

    let on_disk = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(on_disk.contains("id = \"c\""));

    // The device would reload from the watcher; this stands in for it.
    let body = ok(&svc, "reload_config", json!({})).await;
    assert_eq!(body["entry_count"], 3);
}

#[tokio::test]
async fn write_policy_refuses_a_config_that_does_not_parse() {
    let signpost = temp_signpost();
    let (svc, dir) = make_custodial_svc(signpost.path());
    let version = svc.read_policy().unwrap().version;

    let err = svc
        .write_policy("not = valid = toml", Some(&version))
        .unwrap_err();
    assert!(matches!(err, ManagementError::Unprocessable(_)), "{err:?}");

    // And nothing was written: a rejected policy must not cost the device the
    // one it has.
    let on_disk = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(on_disk.contains("id = \"a\""));
}

#[tokio::test]
async fn write_policy_refuses_a_stale_version() {
    let signpost = temp_signpost();
    let (svc, dir) = make_custodial_svc(signpost.path());
    let stale = svc.read_policy().unwrap().version;

    // Someone else — `sudoedit`, or `shepherd install policy` — got there
    // first.
    std::fs::write(dir.path().join("config.toml"), policy_toml(&["z"])).unwrap();

    let err = svc
        .write_policy(&policy_toml(&["a", "b", "c"]), Some(&stale))
        .unwrap_err();
    assert!(matches!(err, ManagementError::Conflict(_)), "{err:?}");
    let on_disk = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(on_disk.contains("id = \"z\""));
}

#[tokio::test]
async fn write_policy_with_no_precondition_overwrites() {
    let signpost = temp_signpost();
    let (svc, dir) = make_custodial_svc(signpost.path());
    svc.write_policy(&policy_toml(&["z"]), None).unwrap();
    let on_disk = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
    assert!(on_disk.contains("id = \"z\""));
}

#[tokio::test]
async fn write_policy_without_a_custodian_writes_the_local_path() {
    let cfg = temp_signpost();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());
    svc.write_policy(&policy_toml(&["a"]), None).unwrap();
    assert!(
        std::fs::read_to_string(cfg.path())
            .unwrap()
            .contains("id = \"a\"")
    );
    let body = ok(&svc, "reload_config", json!({})).await;
    assert_eq!(body["entry_count"], 1);
}

#[tokio::test]
async fn write_policy_is_audited() {
    let signpost = temp_signpost();
    let (svc, _dir) = make_custodial_svc(signpost.path());
    svc.write_policy(&policy_toml(&["a"]), None).unwrap();
    let audits = svc.store.get_recent_audits(10).unwrap();
    assert!(
        audits.iter().any(|a| matches!(
            a.event,
            shepherd_store::AuditEventType::PolicyWritten { entry_count: 1 }
        )),
        "{audits:?}"
    );
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
// Media refresh (issue #165)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn refresh_media_hands_the_prefetcher_a_request() {
    let cfg = temp_config();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);
    let mut svc = make_svc(test_policy(), cfg.path().to_path_buf());
    svc.media_refresh_tx = Some(tx);

    let body = ok(&svc, "refresh_media", json!({})).await;
    assert!(body.is_null(), "no wrap_result, so a bare null: {body}");
    assert_eq!(rx.try_recv(), Ok(()));
}

/// A second press while the first is still queued asks for the same sweep, so
/// it succeeds rather than reporting a conflict — an error there would only
/// teach an administrator to press it again.
#[tokio::test]
async fn a_second_refresh_while_one_is_pending_still_succeeds() {
    let cfg = temp_config();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);
    let mut svc = make_svc(test_policy(), cfg.path().to_path_buf());
    svc.media_refresh_tx = Some(tx);

    ok(&svc, "refresh_media", json!({})).await;
    ok(&svc, "refresh_media", json!({})).await;
    assert_eq!(rx.try_recv(), Ok(()));
    assert!(
        rx.try_recv().is_err(),
        "the duplicate is dropped, not queued"
    );
}

/// Answering "done" to a button that reached nothing is the failure mode this
/// guards: an embedding with no prefetcher has to say so.
#[tokio::test]
async fn refresh_media_without_a_prefetcher_is_unprocessable() {
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());
    assert!(svc.media_refresh_tx.is_none());

    let err = rpc(&svc, "refresh_media", json!({})).await.unwrap_err();
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

// ---------------------------------------------------------------------------
// Audio-output watch loop (issue #124)
// ---------------------------------------------------------------------------

/// Read one `VolumeChanged` if the service emitted one, else `None`.
fn next_volume_event(rx: &mut broadcast::Receiver<Event>) -> Option<(u8, bool, Option<String>)> {
    while let Ok(ev) = rx.try_recv() {
        if let shepherd_api::EventPayload::VolumeChanged {
            percent,
            muted,
            output,
            ..
        } = ev.payload
        {
            return Some((percent, muted, output.map(|o| o.key)));
        }
    }
    None
}

#[tokio::test]
async fn audio_watch_first_tick_only_establishes_a_baseline() {
    let cfg = temp_config();
    let (svc, _vol) = make_svc_with_volume(test_policy(), cfg.path().to_path_buf());
    let mut rx = svc.event_tx.subscribe();

    svc.audio_watch_tick().await;

    // Broadcasting on the first observation would emit a spurious event on every
    // daemon start, when nothing has actually changed.
    assert_eq!(next_volume_event(&mut rx), None);
}

#[tokio::test]
async fn audio_watch_is_silent_while_nothing_changes() {
    let cfg = temp_config();
    let (svc, _vol) = make_svc_with_volume(test_policy(), cfg.path().to_path_buf());
    let mut rx = svc.event_tx.subscribe();

    for _ in 0..5 {
        svc.audio_watch_tick().await;
    }

    // A quiet host must not produce a 2-second event stream.
    assert_eq!(next_volume_event(&mut rx), None);
}

#[tokio::test]
async fn audio_watch_reports_a_volume_change_made_behind_our_back() {
    let cfg = temp_config();
    let (svc, vol) = make_svc_with_volume(test_policy(), cfg.path().to_path_buf());
    svc.audio_watch_tick().await; // baseline
    let mut rx = svc.event_tx.subscribe();

    // Something outside shepherdd moved the volume — a bare `wpctl` call, or a
    // desktop hotkey. Nothing else in the daemon would notice.
    vol.status.lock().unwrap().percent = 77;
    svc.audio_watch_tick().await;

    assert_eq!(next_volume_event(&mut rx), Some((77, false, None)));
}

#[tokio::test]
async fn audio_watch_reports_a_sink_switch_at_an_unchanged_volume() {
    let cfg = temp_config();
    let (svc, vol) = make_svc_with_volume(test_policy(), cfg.path().to_path_buf());
    *vol.output.lock().unwrap() = Some(output(
        "alsa_card.pci-0000_00_1b.0:output:speaker",
        "Speakers",
    ));
    svc.audio_watch_tick().await; // baseline
    let mut rx = svc.event_tx.subscribe();

    // Headphones are plugged in. The percentage happens to be identical, so a
    // watcher keyed only on the reading would stay silent and every client would
    // keep displaying the speakers' state.
    *vol.output.lock().unwrap() = Some(output(
        "alsa_card.pci-0000_00_1b.0:output:analog-output-headphones",
        "Headphones",
    ));
    svc.audio_watch_tick().await;

    assert_eq!(
        next_volume_event(&mut rx),
        Some((
            50,
            false,
            Some("alsa_card.pci-0000_00_1b.0:output:analog-output-headphones".into())
        ))
    );
}

/// Records raised/cleared diagnostics so a test can assert on the condition a
/// parent would actually see.
#[derive(Default)]
struct RecordingSink {
    raised: std::sync::Mutex<Vec<shepherd_api::DiagnosticCode>>,
    cleared: std::sync::Mutex<Vec<shepherd_api::DiagnosticCode>>,
}

impl shepherd_api::DiagnosticSink for RecordingSink {
    fn raise(&self, diagnostic: shepherd_api::Diagnostic) {
        self.raised.lock().unwrap().push(diagnostic.code);
    }
    fn clear(&self, code: shepherd_api::DiagnosticCode, _s: &shepherd_api::DiagnosticSubject) {
        self.cleared.lock().unwrap().push(code);
    }
}

/// A cap must not relax itself because we briefly could not see.
///
/// `volume_restrictions_for(None)` answers with the global limit, so before this
/// fix a failed `pw-dump` dropped the active output's own ceiling and let the
/// volume go to the global maximum for as long as the fault lasted.
#[tokio::test]
async fn a_failed_topology_read_does_not_relax_a_per_output_cap() {
    let cfg = temp_config();
    let mut policy = test_policy();
    policy.volume = VolumePolicy {
        max_volume: Some(80),
        min_volume: None,
        allow_mute: true,
        allow_change: true,
    };
    let (svc, vol) = make_svc_with_volume(policy, cfg.path().to_path_buf());

    let cans = output("card:output:analog-output-headphones", "Headphones");
    vol.plug_in(cans.clone());
    *vol.output.lock().unwrap() = Some(cans.clone());
    svc.audio_watch_tick().await; // establishes what we last saw
    svc.set_audio_output_limits(cans.key.clone(), Some(30), None)
        .await
        .expect("cap accepted");

    vol.break_topology();
    let info = svc.set_volume(80).await.expect("set_volume answered");

    assert_eq!(
        info.percent, 30,
        "a failed read must not raise the headphones' ceiling to the global 80"
    );
    assert_eq!(vol.status.lock().unwrap().percent, 30);
}

/// The parent's device list must not turn into "nothing is plugged in" because
/// one read failed — that is the screen they would use to fix it.
#[tokio::test]
async fn a_failed_topology_read_does_not_report_every_device_as_disconnected() {
    let cfg = temp_config();
    let (svc, vol) = make_svc_with_volume(test_policy(), cfg.path().to_path_buf());

    let speakers = output("card:output:speaker", "Speakers");
    let cans = output("card:output:analog-output-headphones", "Headphones");
    vol.plug_in(speakers.clone());
    vol.plug_in(cans.clone());
    *vol.output.lock().unwrap() = Some(cans.clone());
    svc.audio_watch_tick().await;
    svc.list_audio_outputs().await.expect("rows recorded");

    vol.break_topology();
    let rows = svc.list_audio_outputs().await.expect("rows still answered");

    assert_eq!(rows.len(), 2, "the stored rows survive a failed read");
    assert!(
        rows.iter().all(|r| r.available),
        "a transient read failure must not render as every device being unplugged"
    );
    let active: Vec<_> = rows
        .iter()
        .filter(|r| r.active)
        .map(|r| &r.output.key)
        .collect();
    assert_eq!(
        active,
        vec![&cans.key],
        "the last output known to be in use stays marked, rather than nothing being in use"
    );
}

/// A failed read must not become a baseline, or recovery looks like a change.
#[tokio::test]
async fn a_failed_topology_read_is_not_mistaken_for_an_empty_topology() {
    let cfg = temp_config();
    let (svc, vol) = make_svc_with_volume(test_policy(), cfg.path().to_path_buf());
    vol.plug_in(output("card:output:speaker", "Speakers"));
    *vol.output.lock().unwrap() = Some(output("card:output:speaker", "Speakers"));
    svc.audio_watch_tick().await; // baseline
    let mut rx = svc.event_tx.subscribe();

    vol.break_topology();
    svc.audio_watch_tick().await;
    assert_eq!(
        next_volume_event(&mut rx),
        None,
        "a failed read is not news about the devices"
    );

    // Nothing actually changed while we could not see, so coming back must be
    // silent too. Baselining the empty snapshot would make this a change.
    vol.fix_topology();
    svc.audio_watch_tick().await;
    assert_eq!(
        next_volume_event(&mut rx),
        None,
        "recovering from a failed read is not a device change either"
    );
}

/// The condition is reported while it holds and withdrawn as soon as it does not.
#[tokio::test]
async fn a_failed_topology_read_is_reported_to_the_parent_and_clears_itself() {
    let cfg = temp_config();
    let (mut svc, vol) = make_svc_with_volume(test_policy(), cfg.path().to_path_buf());
    let sink = Arc::new(RecordingSink::default());
    svc.diagnostics = Some(sink.clone() as Arc<dyn shepherd_api::DiagnosticSink>);

    vol.break_topology();
    svc.audio_watch_tick().await;
    assert!(
        sink.raised
            .lock()
            .unwrap()
            .contains(&shepherd_api::DiagnosticCode::AudioTopologyUnreadable),
        "the parent is told the audio devices cannot be read"
    );

    vol.fix_topology();
    svc.audio_watch_tick().await;
    assert!(
        sink.cleared
            .lock()
            .unwrap()
            .contains(&shepherd_api::DiagnosticCode::AudioTopologyUnreadable),
        "and the condition withdraws itself once a read succeeds"
    );
}

#[tokio::test]
async fn volume_event_carries_the_restrictions_in_force() {
    // Regression test for the bug this change exists to fix: subscribers used to
    // receive only `percent`/`muted` and had to keep the restrictions from their
    // initial fetch, so a client could not learn that the applicable limits had
    // changed.
    let cfg = temp_config();
    let mut policy = test_policy();
    policy.volume = VolumePolicy {
        max_volume: Some(60),
        min_volume: Some(10),
        allow_mute: false,
        allow_change: true,
    };
    let (svc, vol) = make_svc_with_volume(policy, cfg.path().to_path_buf());
    svc.audio_watch_tick().await; // baseline
    let mut rx = svc.event_tx.subscribe();

    vol.status.lock().unwrap().percent = 42;
    svc.audio_watch_tick().await;

    let ev = rx.try_recv().expect("an event was broadcast");
    let shepherd_api::EventPayload::VolumeChanged { restrictions, .. } = ev.payload else {
        panic!("expected VolumeChanged");
    };
    assert_eq!(restrictions.max_volume, Some(60));
    assert_eq!(restrictions.min_volume, Some(10));
    assert!(!restrictions.allow_mute);
}

#[tokio::test]
async fn get_volume_reports_the_active_output() {
    let cfg = temp_config();
    let (svc, vol) = make_svc_with_volume(test_policy(), cfg.path().to_path_buf());
    *vol.output.lock().unwrap() = Some(output("alsa_card.usb-x:output:analog-output", "Scarlett"));

    let body = ok(&svc, "get_volume", json!({})).await;

    assert_eq!(
        body["output"]["key"],
        "alsa_card.usb-x:output:analog-output"
    );
    assert_eq!(body["output"]["description"], "Scarlett");
}

// ---------------------------------------------------------------------------
// Per-output volume limits (issue #124)
// ---------------------------------------------------------------------------

/// Put the service on a named output and let the watcher discover it, which is
/// how a row comes to exist at all.
async fn on_output(svc: &DefaultManagementService, vol: &Arc<MockVolume>, key: &str, desc: &str) {
    let o = output(key, desc);
    vol.plug_in(o.clone());
    *vol.output.lock().unwrap() = Some(o);
    svc.audio_watch_tick().await;
}

#[tokio::test]
async fn outputs_are_discovered_by_being_used() {
    let cfg = temp_config();
    let (svc, vol) = make_svc_with_volume(test_policy(), cfg.path().to_path_buf());
    on_output(&svc, &vol, "card:output:headphones", "Cans").await;

    let body = ok(&svc, "list_audio_outputs", json!({})).await;
    let rows = body.as_array().expect("a list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["output"]["key"], "card:output:headphones");
    assert_eq!(rows[0]["output"]["description"], "Cans");
    // No cap until a parent sets one.
    assert!(rows[0]["max_volume"].is_null());
    assert_eq!(rows[0]["active"], true);
}

#[tokio::test]
async fn a_per_output_cap_applies_to_that_output_only() {
    let cfg = temp_config();
    let (svc, vol) = make_svc_with_volume(test_policy(), cfg.path().to_path_buf());
    on_output(&svc, &vol, "card:output:headphones", "Cans").await;
    on_output(&svc, &vol, "card:output:speaker", "Speakers").await;
    ok(
        &svc,
        "set_audio_output_limits",
        json!({ "output_key": "card:output:headphones", "max_volume": 50 }),
    )
    .await;

    on_output(&svc, &vol, "card:output:speaker", "Speakers").await;
    let speakers = ok(&svc, "get_volume", json!({})).await;
    assert!(speakers["restrictions"]["max_volume"].is_null());

    on_output(&svc, &vol, "card:output:headphones", "Cans").await;
    let cans = ok(&svc, "get_volume", json!({})).await;
    assert_eq!(cans["restrictions"]["max_volume"], 50);
}

#[tokio::test]
async fn the_stricter_of_the_policy_and_output_caps_wins() {
    let cfg = temp_config();
    let mut policy = test_policy();
    policy.volume = VolumePolicy {
        max_volume: Some(60),
        min_volume: None,
        allow_mute: true,
        allow_change: true,
    };
    let (svc, vol) = make_svc_with_volume(policy, cfg.path().to_path_buf());
    on_output(&svc, &vol, "k", "Cans").await;

    // Output cap lower than the policy cap: the output wins.
    ok(
        &svc,
        "set_audio_output_limits",
        json!({ "output_key": "k", "max_volume": 50 }),
    )
    .await;
    let body = ok(&svc, "get_volume", json!({})).await;
    assert_eq!(body["restrictions"]["max_volume"], 50);

    // Output cap higher than the policy cap: the policy still wins. A
    // per-output limit must never be usable to raise a limit set elsewhere.
    ok(
        &svc,
        "set_audio_output_limits",
        json!({ "output_key": "k", "max_volume": 90 }),
    )
    .await;
    let body = ok(&svc, "get_volume", json!({})).await;
    assert_eq!(body["restrictions"]["max_volume"], 60);
}

#[tokio::test]
async fn an_unseen_output_inherits_the_global_cap() {
    let cfg = temp_config();
    let mut policy = test_policy();
    policy.volume = VolumePolicy {
        max_volume: Some(80),
        min_volume: None,
        allow_mute: true,
        allow_change: true,
    };
    let (svc, vol) = make_svc_with_volume(policy, cfg.path().to_path_buf());
    *vol.output.lock().unwrap() = Some(output("brand-new", "Just plugged in"));

    let body = ok(&svc, "get_volume", json!({})).await;
    assert_eq!(body["restrictions"]["max_volume"], 80);
}

#[tokio::test]
async fn switching_to_a_capped_output_turns_the_volume_down() {
    let cfg = temp_config();
    let (svc, vol) = make_svc_with_volume(test_policy(), cfg.path().to_path_buf());
    on_output(&svc, &vol, "card:output:headphones", "Cans").await;
    ok(
        &svc,
        "set_audio_output_limits",
        json!({ "output_key": "card:output:headphones", "max_volume": 50 }),
    )
    .await;
    on_output(&svc, &vol, "card:output:speaker", "Speakers").await;
    vol.status.lock().unwrap().percent = 100;

    // Headphones are plugged back in while the speakers were at 100. Bounding
    // only future changes would leave them at 100 — the whole point of the
    // limit is that it bites now.
    on_output(&svc, &vol, "card:output:headphones", "Cans").await;

    assert_eq!(vol.status.lock().unwrap().percent, 50);
}

#[tokio::test]
async fn setting_a_cap_bites_on_the_output_already_playing() {
    let cfg = temp_config();
    let (svc, vol) = make_svc_with_volume(test_policy(), cfg.path().to_path_buf());
    on_output(&svc, &vol, "k", "Cans").await;
    vol.status.lock().unwrap().percent = 90;

    ok(
        &svc,
        "set_audio_output_limits",
        json!({ "output_key": "k", "max_volume": 40 }),
    )
    .await;

    // Otherwise a parent sets a limit, hears no change, and concludes it did
    // not work.
    assert_eq!(vol.status.lock().unwrap().percent, 40);
}

#[tokio::test]
async fn a_quiet_output_is_left_alone_when_a_cap_is_set() {
    let cfg = temp_config();
    let (svc, vol) = make_svc_with_volume(test_policy(), cfg.path().to_path_buf());
    on_output(&svc, &vol, "k", "Cans").await;
    vol.status.lock().unwrap().percent = 30;

    ok(
        &svc,
        "set_audio_output_limits",
        json!({ "output_key": "k", "max_volume": 40 }),
    )
    .await;

    assert_eq!(vol.status.lock().unwrap().percent, 30);
}

#[tokio::test]
async fn limits_survive_the_device_going_away_and_coming_back() {
    let cfg = temp_config();
    let (svc, vol) = make_svc_with_volume(test_policy(), cfg.path().to_path_buf());
    on_output(&svc, &vol, "k", "Cans").await;
    ok(
        &svc,
        "set_audio_output_limits",
        json!({ "output_key": "k", "max_volume": 50 }),
    )
    .await;

    on_output(&svc, &vol, "other", "Something else").await;
    on_output(&svc, &vol, "k", "Cans").await; // unplug, replug

    let body = ok(&svc, "get_volume", json!({})).await;
    assert_eq!(body["restrictions"]["max_volume"], 50);
}

#[tokio::test]
async fn forgetting_an_output_drops_its_cap() {
    let cfg = temp_config();
    let (svc, vol) = make_svc_with_volume(test_policy(), cfg.path().to_path_buf());
    on_output(&svc, &vol, "k", "Cans").await;
    ok(
        &svc,
        "set_audio_output_limits",
        json!({ "output_key": "k", "max_volume": 50 }),
    )
    .await;

    let removed = ok(&svc, "forget_audio_output", json!({ "output_key": "k" })).await;
    assert_eq!(removed, json!(true));

    let body = ok(&svc, "get_volume", json!({})).await;
    assert!(body["restrictions"]["max_volume"].is_null());
}

#[tokio::test]
async fn limits_on_an_unknown_output_are_rejected() {
    let cfg = temp_config();
    let (svc, _vol) = make_svc_with_volume(test_policy(), cfg.path().to_path_buf());
    let err = rpc(
        &svc,
        "set_audio_output_limits",
        json!({ "output_key": "never-seen", "max_volume": 50 }),
    )
    .await
    .unwrap_err();
    assert!(
        format!("{err:?}").contains("Unknown audio output"),
        "got {err:?}"
    );
}

#[tokio::test]
async fn nonsensical_limits_are_rejected() {
    let cfg = temp_config();
    let (svc, vol) = make_svc_with_volume(test_policy(), cfg.path().to_path_buf());
    on_output(&svc, &vol, "k", "Cans").await;

    for params in [
        json!({ "output_key": "k", "max_volume": 150 }),
        json!({ "output_key": "k", "max_volume": 40, "min_volume": 60 }),
    ] {
        assert!(
            rpc(&svc, "set_audio_output_limits", params.clone())
                .await
                .is_err(),
            "should have rejected {params}"
        );
    }
}

// ---------------------------------------------------------------------------
// Choosing the active output (issue #124)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_connected_output_can_be_listed_before_it_is_ever_selected() {
    let cfg = temp_config();
    let (svc, vol) = make_svc_with_volume(test_policy(), cfg.path().to_path_buf());
    on_output(&svc, &vol, "card:output:speaker", "Speakers").await;
    // Plugged in, never selected. Discovery used to run only off the active
    // output, so this device could not be seen — and a device you cannot see is
    // one you cannot choose.
    vol.plug_in(output("usb:output:analog-output", "USB interface"));

    let rows = ok(&svc, "list_audio_outputs", json!({})).await;
    let rows = rows.as_array().expect("a list");
    assert_eq!(rows.len(), 2);
    let usb = rows
        .iter()
        .find(|r| r["output"]["key"] == "usb:output:analog-output")
        .expect("the unselected device is listed");
    assert_eq!(usb["active"], false);
    assert_eq!(usb["available"], true);
}

#[tokio::test]
async fn rows_report_whether_the_device_is_still_connected() {
    let cfg = temp_config();
    let (svc, vol) = make_svc_with_volume(test_policy(), cfg.path().to_path_buf());
    on_output(&svc, &vol, "card:output:speaker", "Speakers").await;
    on_output(&svc, &vol, "usb:output:analog-output", "USB interface").await;
    // Unplug the USB device and land back on the built-in, as WirePlumber would.
    vol.unplug("usb:output:analog-output");
    on_output(&svc, &vol, "card:output:speaker", "Speakers").await;

    let rows = ok(&svc, "list_audio_outputs", json!({})).await;
    let rows = rows.as_array().expect("a list");
    let by_key = |k: &str| {
        rows.iter()
            .find(|r| r["output"]["key"] == k)
            .expect("row present")
            .clone()
    };
    // The row survives so its cap can still be set, but it cannot be chosen.
    assert_eq!(by_key("usb:output:analog-output")["available"], false);
    assert_eq!(by_key("card:output:speaker")["available"], true);
}

#[tokio::test]
async fn choosing_an_output_moves_the_audio_to_it() {
    let cfg = temp_config();
    let (svc, vol) = make_svc_with_volume(test_policy(), cfg.path().to_path_buf());
    on_output(&svc, &vol, "card:output:speaker", "Speakers").await;
    vol.plug_in(output("usb:output:analog-output", "USB interface"));

    let body = ok(
        &svc,
        "select_audio_output",
        json!({ "output_key": "usb:output:analog-output" }),
    )
    .await;
    assert_eq!(body["output"]["key"], "usb:output:analog-output");
    assert_eq!(
        vol.output.lock().unwrap().as_ref().map(|o| o.key.clone()),
        Some("usb:output:analog-output".into())
    );

    let rows = ok(&svc, "list_audio_outputs", json!({})).await;
    let active: Vec<_> = rows
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["active"] == true)
        .map(|r| r["output"]["key"].clone())
        .collect();
    assert_eq!(active, vec!["usb:output:analog-output"]);
}

#[tokio::test]
async fn choosing_a_capped_output_turns_the_volume_down_on_arrival() {
    let cfg = temp_config();
    let (svc, vol) = make_svc_with_volume(test_policy(), cfg.path().to_path_buf());
    on_output(&svc, &vol, "card:output:headphones", "Cans").await;
    ok(
        &svc,
        "set_audio_output_limits",
        json!({ "output_key": "card:output:headphones", "max_volume": 30 }),
    )
    .await;
    on_output(&svc, &vol, "card:output:speaker", "Speakers").await;
    vol.status.lock().unwrap().percent = 100;

    // Choosing an output has to be the same event as the hardware choosing it:
    // the cap applies on arrival, not on the next change someone makes.
    let body = ok(
        &svc,
        "select_audio_output",
        json!({ "output_key": "card:output:headphones" }),
    )
    .await;
    assert_eq!(body["percent"], 30);
    assert_eq!(body["restrictions"]["max_volume"], 30);
    assert_eq!(vol.status.lock().unwrap().percent, 30);
}

#[tokio::test]
async fn choosing_the_output_already_in_use_changes_nothing() {
    let cfg = temp_config();
    let (svc, vol) = make_svc_with_volume(test_policy(), cfg.path().to_path_buf());
    on_output(&svc, &vol, "card:output:speaker", "Speakers").await;
    vol.status.lock().unwrap().percent = 55;

    // Two parents on two phones can tap the same row; the second one is not an
    // error and must not disturb the volume.
    let body = ok(
        &svc,
        "select_audio_output",
        json!({ "output_key": "card:output:speaker" }),
    )
    .await;
    assert_eq!(body["output"]["key"], "card:output:speaker");
    assert_eq!(body["percent"], 55);
}

#[tokio::test]
async fn an_output_that_is_not_connected_cannot_be_chosen() {
    let cfg = temp_config();
    let (svc, vol) = make_svc_with_volume(test_policy(), cfg.path().to_path_buf());
    on_output(&svc, &vol, "card:output:speaker", "Speakers").await;
    on_output(&svc, &vol, "usb:output:analog-output", "USB interface").await;
    vol.unplug("usb:output:analog-output");
    on_output(&svc, &vol, "card:output:speaker", "Speakers").await;

    let err = rpc(
        &svc,
        "select_audio_output",
        json!({ "output_key": "usb:output:analog-output" }),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            err,
            RpcDispatchError::Management(ManagementError::BadRequest(_))
        ),
        "expected BadRequest, got {err:?}"
    );
    // And the audio stayed where it was.
    assert_eq!(
        vol.output.lock().unwrap().as_ref().map(|o| o.key.clone()),
        Some("card:output:speaker".into())
    );
}

#[tokio::test]
async fn the_watcher_notices_a_device_that_never_becomes_the_default() {
    let cfg = temp_config();
    let (svc, vol) = make_svc_with_volume(test_policy(), cfg.path().to_path_buf());
    on_output(&svc, &vol, "card:output:speaker", "Speakers").await;
    let mut rx = svc.event_tx.subscribe();

    // A second device is plugged in but does not win the default sink — a lower
    // priority interface, or one the user had already pinned away from. Watching
    // only the selected output would miss it entirely, and the row a parent
    // needs in order to switch to it would never appear.
    vol.plug_in(output("usb:output:analog-output", "USB interface"));
    svc.audio_watch_tick().await;

    assert!(
        next_volume_event(&mut rx).is_some(),
        "a device appearing is a change worth telling the clients about"
    );
    let rows = ok(&svc, "list_audio_outputs", json!({})).await;
    let rows = rows.as_array().expect("a list");
    assert_eq!(rows.len(), 2);
    let usb = rows
        .iter()
        .find(|r| r["output"]["key"] == "usb:output:analog-output")
        .expect("the new device was recorded by the watcher, not by the listing");
    assert_eq!(usb["active"], false);
    assert_eq!(usb["available"], true);
}

#[tokio::test]
async fn the_watcher_notices_a_device_being_unplugged() {
    let cfg = temp_config();
    let (svc, vol) = make_svc_with_volume(test_policy(), cfg.path().to_path_buf());
    on_output(&svc, &vol, "card:output:speaker", "Speakers").await;
    vol.plug_in(output("usb:output:analog-output", "USB interface"));
    svc.audio_watch_tick().await;
    let mut rx = svc.event_tx.subscribe();

    vol.unplug("usb:output:analog-output");
    svc.audio_watch_tick().await;

    assert!(next_volume_event(&mut rx).is_some());
    let rows = ok(&svc, "list_audio_outputs", json!({})).await;
    let usb = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["output"]["key"] == "usb:output:analog-output")
        .expect("the row outlives the hardware so its cap survives");
    assert_eq!(usb["available"], false);
}

#[tokio::test]
async fn a_reordered_output_list_is_not_a_change() {
    let cfg = temp_config();
    let (svc, vol) = make_svc_with_volume(test_policy(), cfg.path().to_path_buf());
    on_output(&svc, &vol, "card:output:speaker", "Speakers").await;
    vol.plug_in(output("usb:output:analog-output", "USB interface"));
    svc.audio_watch_tick().await;
    let mut rx = svc.event_tx.subscribe();

    // `pw-dump` lists objects in whatever order it walks them; the same set in a
    // different order must not read as a plug event every two seconds.
    vol.present.lock().unwrap().reverse();
    svc.audio_watch_tick().await;

    assert_eq!(next_volume_event(&mut rx), None);
}

// ---------------------------------------------------------------------------
// Network status (issue #182)
// ---------------------------------------------------------------------------

/// A device with one wireless interface carrying a real address, one container
/// bridge, and loopback — the shape of the machine this was written on.
fn a_device_on_wifi() -> NetworkSnapshot {
    NetworkSnapshot {
        connectivity: Connectivity::Full,
        source: NetworkSource::NetworkManager,
        interfaces: vec![
            NetworkInterfaceView {
                name: "lo".into(),
                kind: NetworkInterfaceKind::Loopback,
                up: true,
                addresses: vec![address("127.0.0.1", 8)],
                gateway: None,
                dns: vec![],
                wifi: None,
                reachable: false,
            },
            NetworkInterfaceView {
                name: "lxcbr0".into(),
                kind: NetworkInterfaceKind::Bridge,
                up: true,
                addresses: vec![address("10.0.3.1", 24)],
                gateway: None,
                dns: vec![],
                wifi: None,
                reachable: false,
            },
            NetworkInterfaceView {
                name: "wlan0".into(),
                kind: NetworkInterfaceKind::Wifi,
                up: true,
                addresses: vec![address("192.168.0.139", 24)],
                gateway: Some("192.168.0.1".into()),
                dns: vec!["192.168.0.1".into()],
                wifi: Some(WifiView {
                    ssid: Some("Home".into()),
                    signal_percent: Some(60),
                    frequency_mhz: Some(5_220),
                }),
                reachable: false,
            },
        ],
    }
}

fn address(address: &str, prefix: u8) -> NetworkAddressView {
    NetworkAddressView {
        address: address.into(),
        prefix,
        family: AddressFamily::V4,
    }
}

/// A service that can see the given network, with a web listener in the given
/// state.
fn make_svc_with_network(
    snapshot: NetworkSnapshot,
    listener: WebListenerHandle,
    config_path: PathBuf,
) -> DefaultManagementService {
    DefaultManagementService {
        network: Some(Arc::new(StaticNetworkInfo(snapshot))),
        web_listener: listener,
        ..make_svc(test_policy(), config_path)
    }
}

fn listening_on_everything() -> WebListenerHandle {
    let handle = WebListenerHandle::configured("0.0.0.0:8080".parse().unwrap(), false);
    handle.set_listening("0.0.0.0:8080".parse().unwrap(), false);
    handle
}

#[tokio::test]
async fn network_status_leads_with_the_address_somebody_can_reach() {
    let cfg = temp_config();
    let svc = make_svc_with_network(
        a_device_on_wifi(),
        listening_on_everything(),
        cfg.path().to_path_buf(),
    );

    let status = ok(&svc, "network_status", json!({})).await;

    assert_eq!(status["connectivity"], "full");
    assert_eq!(status["source"], "network_manager");
    // The whole point of the ticket: a phone that reached this device over BLE
    // now has a URL it can open, without arp-ing the LAN for it.
    assert_eq!(
        status["management_urls"],
        json!(["http://192.168.0.139:8080"])
    );
    let interfaces = status["interfaces"].as_array().unwrap();
    assert_eq!(
        interfaces[0]["name"], "wlan0",
        "the reachable interface sorts first, not the alphabetical one"
    );
    assert_eq!(interfaces[0]["reachable"], true);
    assert_eq!(interfaces[0]["wifi"]["ssid"], "Home");
    assert!(
        interfaces
            .iter()
            .all(|i| i["name"] == "wlan0" || i["reachable"] == false),
        "loopback and a container bridge are not ways in"
    );
}

#[tokio::test]
async fn a_web_interface_that_never_bound_says_so_instead_of_offering_a_url() {
    let cfg = temp_config();
    let failed = WebListenerHandle::configured("10.147.17.8:8080".parse().unwrap(), false);
    failed.set_failed("Cannot assign requested address");
    let svc = make_svc_with_network(a_device_on_wifi(), failed, cfg.path().to_path_buf());

    let status = ok(&svc, "network_status", json!({})).await;

    assert_eq!(status["management_api"]["state"], "failed");
    assert_eq!(status["management_api"]["addr"], "10.147.17.8:8080");
    assert_eq!(
        status["management_api"]["error"],
        "Cannot assign requested address"
    );
    assert_eq!(
        status["management_urls"],
        json!([]),
        "an address that will refuse the connection sends somebody to debug \
         the wrong machine"
    );
}

#[tokio::test]
async fn a_host_that_cannot_look_says_unavailable_rather_than_offline() {
    // A device with no NetworkManager and no readable interfaces is not a
    // device with no network, and a UI must be able to tell them apart.
    let cfg = temp_config();
    let svc = make_svc(test_policy(), cfg.path().to_path_buf());

    let status = ok(&svc, "network_status", json!({})).await;

    assert_eq!(status["source"], "unavailable");
    assert_eq!(status["connectivity"], "unknown");
    assert_eq!(status["interfaces"], json!([]));
    assert_eq!(status["management_api"]["state"], "disabled");
}
