//! Regression tests for the activity-supervision escapes in issues #135/#136.
//!
//! These pin the contract between the engine's session lifetime and the host's
//! process lifetime, which is what both issues violate.
//!
//! Reconstructed from the `copernicus` journal of 2026-08-20; see
//! `docs/ai/history/2026-08-20 001 activity-supervision-escapes.md`.

use shepherd_api::{EntryKind, Event, EventPayload, StopMode};
use shepherd_config::{
    AutoBrightnessPolicy, AvailabilityPolicy, BrightnessPolicy, Entry, LimitsPolicy, Policy,
    ServiceConfig, VolumePolicy,
};
use shepherd_core::CoreEngine;
use shepherd_host_api::{
    HostCapabilities, MockHost, NoOpBrightnessController, NoOpDisplayController,
    NoOpHidpiController, NoOpVolumeController,
};
use shepherd_management::{
    AutoBrightnessState, DefaultManagementService, LaunchOutcome, ManagementService,
};
use shepherd_store::SqliteStore;
use shepherd_util::EntryId;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, broadcast, watch};

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

fn entry(id: &str) -> Entry {
    Entry {
        id: EntryId::new(id),
        label: id.into(),
        icon_ref: None,
        kind: EntryKind::Process {
            command: id.into(),
            args: vec![],
            env: HashMap::new(),
            cwd: None,
        },
        availability: AvailabilityPolicy {
            windows: vec![],
            always: true,
        },
        limits: LimitsPolicy {
            max_run: None,
            daily_quota: None,
            cooldown: None,
            cooldown_min_session: Duration::ZERO,
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
    }
}

/// Two entries, mirroring the tetris → bitwig-studio sequence from the journal.
fn test_policy() -> Policy {
    Policy {
        service: ServiceConfig::default(),
        groups: vec![],
        entries: vec![entry("tetris"), entry("bitwig-studio")],
        default_warnings: vec![],
        default_max_run: None,
        volume: VolumePolicy::unrestricted(),
        brightness: BrightnessPolicy::default(),
        auto_brightness: AutoBrightnessPolicy::default(),
    }
}

struct Harness {
    svc: DefaultManagementService,
    events: broadcast::Receiver<Event>,
}

fn harness() -> Harness {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let host = Arc::new(MockHost::new());
    let engine = Arc::new(Mutex::new(CoreEngine::new(
        test_policy(),
        store.clone(),
        HostCapabilities::minimal(),
    )));
    let (tx, events) = broadcast::channel::<Event>(64);
    let tx_for_fn = tx.clone();
    let (shutdown_tx, _shutdown_rx) = watch::channel(false);

    let svc = DefaultManagementService {
        engine,
        store,
        host,
        volume: Arc::new(NoOpVolumeController::default()),
        brightness: Arc::new(NoOpBrightnessController::default()),
        light_sensor: None,
        auto_brightness: Arc::new(Mutex::new(AutoBrightnessState::new(false))),
        event_tx: tx,
        broadcast_fn: Arc::new(move |event: Event| {
            let _ = tx_for_fn.send(event);
        }),
        config_path: std::path::PathBuf::from("/nonexistent/config.toml"),
        shutdown_tx,
        hidpi: Arc::new(NoOpHidpiController),
        display: Arc::new(NoOpDisplayController),
    };

    Harness { svc, events }
}

/// Drain the broadcast channel into the payloads seen so far.
fn drained(rx: &mut broadcast::Receiver<Event>) -> Vec<EventPayload> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        out.push(ev.payload);
    }
    out
}

async fn launch(svc: &DefaultManagementService, id: &str) {
    match svc.launch(EntryId::new(id)).await.unwrap() {
        LaunchOutcome::Approved { .. } => {}
        LaunchOutcome::Denied { reasons } => panic!("{id} unexpectedly denied: {reasons:?}"),
    }
}

// ---------------------------------------------------------------------------
// #136 — escape on close
// ---------------------------------------------------------------------------

/// A late exit event from the *previous* activity must not end the session that
/// replaced it.
///
/// Journal, 19:48:42: RetroArch's SIGKILL reap was noticed 24ms after Bitwig's
/// session started, and `notify_session_exited` — which ignores the handle
/// entirely — ended Bitwig instead. Audit log recorded `bitwig-studio`,
/// `duration 0s`, `exit_code: null`, i.e. RetroArch's `signal 9`.
#[tokio::test]
async fn stale_exit_from_previous_activity_does_not_end_the_next_session() {
    let mut h = harness();

    launch(&h.svc, "tetris").await;
    let tetris_handle = {
        let eng = h.svc.engine.lock().await;
        eng.current_session()
            .and_then(|s| s.host_handle.clone())
            .expect("tetris has a host handle")
    };

    // Tetris is stopped; its reap is still in flight.
    let _ = h.svc.stop_current(StopMode::Graceful).await;

    // The misrouted press lands: the next activity starts.
    launch(&h.svc, "bitwig-studio").await;
    let bitwig_session = {
        let eng = h.svc.engine.lock().await;
        eng.current_session()
            .map(|s| s.plan.session_id.clone())
            .expect("bitwig session is active")
    };

    // Now tetris's exit finally surfaces. This is the call `shepherdd`'s
    // `handle_host_event` makes for *every* `HostEvent::Exited`, with no way
    // to say which activity it describes — the defect under test.
    let ended = {
        let mut eng = h.svc.engine.lock().await;
        eng.notify_activity_exited(
            &tetris_handle,
            None,
            shepherd_util::MonotonicInstant::now(),
            shepherd_util::now(),
        )
    };

    assert!(
        ended.is_none(),
        "an exit belonging to the previous activity ended the current session"
    );
    let eng = h.svc.engine.lock().await;
    assert_eq!(
        eng.current_session().map(|s| s.plan.session_id.clone()),
        Some(bitwig_session),
        "bitwig's session must survive RetroArch's reap"
    );
    let _ = drained(&mut h.events);
}
