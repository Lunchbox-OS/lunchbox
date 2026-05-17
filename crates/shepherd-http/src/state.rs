//! Shared application state for HTTP handlers

use shepherd_api::Event;
use shepherd_core::CoreEngine;
use shepherd_host_api::{HidpiController, HostAdapter, VolumeController};
use shepherd_store::Store;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast, watch};

#[derive(Clone)]
pub struct AppState {
    pub engine: Arc<Mutex<CoreEngine>>,
    pub store: Arc<dyn Store>,
    pub host: Arc<dyn HostAdapter>,
    pub volume: Arc<dyn VolumeController>,
    pub event_tx: broadcast::Sender<Event>,
    /// Broadcasts an event to all subscribers: both IPC clients (HUD) and HTTP SSE clients.
    /// Equivalent to calling the daemon's internal `broadcast()` helper.
    pub broadcast_fn: Arc<dyn Fn(Event) + Send + Sync>,
    pub config_path: PathBuf,
    /// Fires when shepherdd should begin graceful shutdown. The logout handler
    /// flips this to `true`; the daemon's main loop and the HTTP server's
    /// `with_graceful_shutdown` future both observe it.
    pub shutdown_tx: watch::Sender<bool>,
    /// XWayland HiDPI scale-toggle controller (issue #45). The HTTP launch
    /// path uses this to apply/restore the workaround for entries that set
    /// `xwayland_native_resolution = true`, in parity with the IPC launch
    /// path. Defaults to `NoOpHidpiController` in tests.
    pub hidpi: Arc<dyn HidpiController>,
}
