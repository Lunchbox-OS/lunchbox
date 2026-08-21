//! Regression tests for the activity-supervision escapes in issues #135/#136.
//!
//! These pin the contract between the engine's session lifetime and the host's
//! process lifetime, which is what both issues violate.
//!
//! Reconstructed from the `copernicus` journal of 2026-08-20; see
//! `docs/ai/history/2026-08-20 001 activity-supervision-escapes.md`.

use async_trait::async_trait;
use shepherd_api::{EntryKind, Event, EventPayload, StopMode};
use shepherd_config::{
    AutoBrightnessPolicy, AvailabilityPolicy, BrightnessPolicy, Entry, LimitsPolicy, Policy,
    ServiceConfig, VolumePolicy,
};
use shepherd_core::CoreEngine;
use shepherd_host_api::{
    HidpiController, HostCapabilities, MockHost, NoOpBrightnessController, NoOpDisplayController,
    NoOpVolumeController,
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

/// Records the order in which the service touches the compositor, so tests can
/// assert that the launcher is restored *after* teardown rather than before.
#[derive(Default)]
struct RecordingHidpi {
    log: Arc<std::sync::Mutex<Vec<&'static str>>>,
}

#[async_trait]
impl HidpiController for RecordingHidpi {
    async fn apply(&self) {
        self.log.lock().unwrap().push("hidpi.apply");
    }
    async fn restore(&self) {
        self.log.lock().unwrap().push("hidpi.restore");
    }
    async fn factor(&self) -> f64 {
        1.0
    }
}

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
    host: Arc<MockHost>,
    events: broadcast::Receiver<Event>,
    hidpi_log: Arc<std::sync::Mutex<Vec<&'static str>>>,
}

fn harness() -> Harness {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    let host = Arc::new(MockHost::new());
    let hidpi_log: Arc<std::sync::Mutex<Vec<&'static str>>> = Default::default();
    let hidpi = Arc::new(RecordingHidpi {
        log: hidpi_log.clone(),
    });
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
        host: host.clone(),
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
        hidpi,
        display: Arc::new(NoOpDisplayController),
    };

    Harness {
        svc,
        host,
        events,
        hidpi_log,
    }
}

/// Drain the broadcast channel into the payloads seen so far.
fn drained(rx: &mut broadcast::Receiver<Event>) -> Vec<EventPayload> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        out.push(ev.payload);
    }
    out
}

fn has_session_ended(payloads: &[EventPayload]) -> bool {
    payloads
        .iter()
        .any(|p| matches!(p, EventPayload::SessionEnded { .. }))
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

/// The launcher must not be told the session is over while the activity is
/// still being torn down.
///
/// Journal, 19:48:37–19:48:42: `Session stopped` and its `SessionEnded`
/// broadcast landed at t=0, but RetroArch ran on for another 5.1s. The child
/// saw an interactive grid over a live activity and their next press launched
/// Bitwig by accident.
#[tokio::test]
async fn session_end_is_not_announced_until_teardown_finishes() {
    let mut h = harness();
    h.host
        .set_late_reap(Duration::from_millis(300), Duration::ZERO);

    launch(&h.svc, "tetris").await;
    let _ = drained(&mut h.events);

    let stopping = h.svc.stop_current(StopMode::Graceful);
    tokio::pin!(stopping);

    // Drive the stop for a while without letting it finish, so we can look at
    // what the rest of the system was told mid-teardown.
    tokio::select! {
        r = &mut stopping => panic!("stop finished before the mock released it: {r:?}"),
        _ = tokio::time::sleep(Duration::from_millis(150)) => {}
    }
    let midway = drained(&mut h.events);
    assert!(
        !has_session_ended(&midway),
        "SessionEnded was broadcast while the activity was still being stopped; \
         this is what put an interactive launcher over a live RetroArch"
    );

    stopping.await.unwrap();
    let after = drained(&mut h.events);
    assert!(
        has_session_ended(&after),
        "SessionEnded must be broadcast once teardown completes"
    );
}

/// The compositor must not be handed back to the launcher before the outgoing
/// activity is gone.
///
/// Journal: `Restored sway output scales` at 19:48:37.161 — 40ms *before*
/// SIGTERM was even sent, while RetroArch's XWayland window was still mapped.
#[tokio::test]
async fn hidpi_is_restored_after_teardown_not_before() {
    let mut h = harness();
    h.host
        .set_late_reap(Duration::from_millis(300), Duration::ZERO);

    launch(&h.svc, "tetris").await;
    h.hidpi_log.lock().unwrap().clear();

    let stopping = h.svc.stop_current(StopMode::Graceful);
    tokio::pin!(stopping);

    tokio::select! {
        r = &mut stopping => panic!("stop finished before the mock released it: {r:?}"),
        _ = tokio::time::sleep(Duration::from_millis(150)) => {}
    }
    assert!(
        h.hidpi_log.lock().unwrap().is_empty(),
        "output scale was restored while the activity was still mapped"
    );

    stopping.await.unwrap();
    assert_eq!(
        h.hidpi_log.lock().unwrap().clone(),
        vec!["hidpi.restore"],
        "expected exactly one restore, after teardown"
    );
    let _ = drained(&mut h.events);
}

/// Nothing else may launch while the outgoing activity is still being torn
/// down. This is the assertion that actually protects the child.
///
/// On 2026-08-20 the close was pressed on a gamepad. The launcher polls
/// gamepads straight from evdev via `gilrs` on a 16ms timer
/// (`shepherd-launcher-ui/src/app.rs`), so those presses never go through the
/// compositor and are not gated by window focus at all — the second press
/// reached the grid and `launch_selected()` fired regardless of what was on
/// screen. Refusing the launch in the engine is therefore the only layer that
/// covers every input path.
#[tokio::test]
async fn nothing_can_launch_while_the_previous_activity_is_still_being_stopped() {
    let mut h = harness();
    h.host
        .set_late_reap(Duration::from_millis(400), Duration::ZERO);

    launch(&h.svc, "tetris").await;
    let _ = drained(&mut h.events);

    let stopping = h.svc.stop_current(StopMode::Graceful);
    tokio::pin!(stopping);
    tokio::select! {
        r = &mut stopping => panic!("stop finished before the mock released it: {r:?}"),
        _ = tokio::time::sleep(Duration::from_millis(150)) => {}
    }

    // The stray press lands mid-teardown.
    let outcome = h.svc.launch(EntryId::new("bitwig-studio")).await.unwrap();
    match outcome {
        LaunchOutcome::Denied { .. } => {}
        LaunchOutcome::Approved { .. } => panic!(
            "an activity launched while the previous one was still running — \
             exactly the accidental Bitwig launch from the journal"
        ),
    }

    stopping.await.unwrap();

    // And once teardown really is done, launching works again.
    let after = h.svc.launch(EntryId::new("bitwig-studio")).await.unwrap();
    assert!(
        matches!(after, LaunchOutcome::Approved { .. }),
        "launching must be possible again once the activity is gone"
    );
    let _ = drained(&mut h.events);
}

/// While teardown is in flight the session must report itself as `Stopping`,
/// so shells can show a closing state instead of an unchanged screen.
///
/// The child on 2026-08-20 pressed close, saw nothing change, and pressed
/// again. Holding the session (the fix above) stops the second press doing
/// damage; this is what stops it happening in the first place.
#[tokio::test]
async fn the_session_reports_itself_as_stopping_during_teardown() {
    let mut h = harness();
    h.host
        .set_late_reap(Duration::from_millis(400), Duration::ZERO);

    launch(&h.svc, "tetris").await;
    assert_eq!(
        h.svc.current_session().await.map(|s| s.state),
        Some(shepherd_api::SessionState::Running)
    );

    let stopping = h.svc.stop_current(StopMode::Graceful);
    tokio::pin!(stopping);
    tokio::select! {
        r = &mut stopping => panic!("stop finished before the mock released it: {r:?}"),
        _ = tokio::time::sleep(Duration::from_millis(150)) => {}
    }

    assert_eq!(
        h.svc.current_session().await.map(|s| s.state),
        Some(shepherd_api::SessionState::Stopping),
        "shells need this to render a closing state"
    );

    stopping.await.unwrap();
    assert!(
        h.svc.current_session().await.is_none(),
        "the session is gone once teardown completes"
    );
    let _ = drained(&mut h.events);
}

/// A stop that did not actually stop anything must be reported as a failure,
/// not swallowed.
///
/// `LinuxHost::stop` sends SIGKILL at the timeout and returns `Ok(())` without
/// re-checking, and `stop_current` discards the result with `let _`.
#[tokio::test]
async fn stop_reports_failure_when_the_activity_survives() {
    let mut h = harness();
    h.host.set_unkillable(Duration::from_millis(100));

    launch(&h.svc, "tetris").await;
    let _ = drained(&mut h.events);

    let result = h.svc.stop_current(StopMode::Graceful).await;
    assert!(
        result.is_err(),
        "stop_current reported success for an activity that is still running"
    );
    assert_eq!(
        h.host.running_sessions().len(),
        1,
        "the activity really did survive, so the harness models #136 correctly"
    );
}

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
