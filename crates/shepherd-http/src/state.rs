//! Shared application state for HTTP handlers

use shepherd_api::Event;
use shepherd_core::CoreEngine;
use shepherd_host_api::{HostAdapter, VolumeController};
use shepherd_store::Store;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast};

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
}
