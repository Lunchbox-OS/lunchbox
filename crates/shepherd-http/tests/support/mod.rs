//! A management service that exists only so an `AppState` can be built.
//!
//! `tests/files.rs` tests routes that never call the trait — the file manager
//! is its own field on `AppState`, deliberately, because nothing about it
//! reaches BLE or the companion. It still needs *a* service to construct the
//! state around, so this is the smallest one that compiles: no-op controllers,
//! an in-memory store, and a mock host.
//!
//! Deliberately not shared with `tests/api.rs`, which needs mocks that
//! actually record what they were told — this one would make those tests worse
//! rather than shorter.

use std::path::PathBuf;
use std::sync::Arc;

use shepherd_api::Event;
use shepherd_core::CoreEngine;
use shepherd_host_api::{
    HostCapabilities, MockHost, NoOpBrightnessController, NoOpDisplayController,
    NoOpHidpiController, NoOpHudLayoutController, NoOpVolumeController,
};
use shepherd_management::{
    AutoBrightnessState, DefaultManagementService, ManagementService, WebListenerHandle,
};
use shepherd_store::SqliteStore;
use tokio::sync::{Mutex, broadcast, watch};

pub fn make_service() -> Arc<dyn ManagementService> {
    let store = Arc::new(SqliteStore::in_memory().unwrap());
    // The smallest policy there is: this service answers nothing these tests
    // ask, and a policy with entries in it would only invite someone to think
    // it did.
    let policy = shepherd_config::parse_config("config_version = 1\n").unwrap();
    let engine = Arc::new(Mutex::new(CoreEngine::new(
        policy,
        store.clone(),
        HostCapabilities::minimal(),
    )));
    let (tx, _) = broadcast::channel::<Event>(16);
    let tx_for_fn = tx.clone();
    let (shutdown_tx, _shutdown_rx) = watch::channel(false);
    Arc::new(DefaultManagementService {
        engine,
        store,
        host: Arc::new(MockHost::new()),
        volume: Arc::new(NoOpVolumeController::default()),
        brightness: Arc::new(NoOpBrightnessController::default()),
        light_sensor: None,
        auto_brightness: Arc::new(Mutex::new(AutoBrightnessState::new(false))),
        event_tx: tx,
        broadcast_fn: Arc::new(move |event: Event| {
            let _ = tx_for_fn.send(event);
        }),
        config_path: PathBuf::from("/nonexistent/config.toml"),
        policy_files: None,
        media_refresh_tx: None,
        shutdown_tx,
        hidpi: Arc::new(NoOpHidpiController),
        hud_layout: Arc::new(NoOpHudLayoutController),
        display: Arc::new(NoOpDisplayController),
        last_audio_state: Arc::new(Mutex::new(None)),
        diagnostics: None,
        network: None,
        web_listener: WebListenerHandle::default(),
        web_auth: None,
        // No BLE in this fixture, so no roster of administrators to list
        // (issue #149).
        admins: Default::default(),
    })
}
