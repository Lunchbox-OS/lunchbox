//! Sway output-hotplug watcher for docking support (issue #87).
//!
//! Subscribes to sway's `output` events and nudges the [`DisplayManager`] to
//! reconcile whenever a display is connected, disconnected, or reconfigured.
//! Reconciliation is idempotent, so the burst of events sway emits for a
//! single hotplug collapses to at most one arrangement change.
//!
//! This used to spawn `swaymsg -t subscribe -m '["output"]'` and read its
//! stdout, and restart it whenever the subprocess died. It now holds the IPC
//! connection itself (issue #147), which also means the subscription is
//! established — and can be seen to have failed — before this returns, rather
//! than inside a task nobody is waiting on.
//!
//! There is deliberately no respawn loop any more. lunchboxd is `exec`'d by
//! sway and dies with it, so an event stream that ends means the session is
//! ending, not that the compositor will be back.

use crate::display::DisplayManager;
use lunchbox_host_linux::sway_ipc::Subscription;
use std::sync::Arc;
use tokio::sync::watch;
use tracing::{info, warn};

/// Subscribe to sway's output events and spawn the task that acts on them.
///
/// The subscription is opened here rather than inside the task so a compositor
/// that cannot be reached is reported to the caller, and so the connection
/// exists before anything downstream depends on it.
pub async fn spawn(
    manager: Arc<DisplayManager>,
    mut shutdown_rx: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    let subscription = match Subscription::open(&["output"]).await {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "Failed to subscribe to sway output events; docking hotplug disabled");
            return tokio::spawn(async {});
        }
    };
    info!("Watching sway output events for docking");

    tokio::spawn(async move {
        let mut subscription = subscription;
        loop {
            tokio::select! {
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        return;
                    }
                }
                event = subscription.next_event() => {
                    match event {
                        // Each event is one output change (connect / disconnect
                        // / mode change). We don't inspect the payload —
                        // reconcile re-queries the full topology.
                        Ok(_) => manager.on_output_changed().await,
                        Err(e) => {
                            warn!(error = %e, "Sway output event stream ended; docking hotplug is over");
                            return;
                        }
                    }
                }
            }
        }
    })
}
