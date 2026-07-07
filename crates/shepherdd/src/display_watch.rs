//! Sway output-hotplug watcher for docking support (issue #87).
//!
//! Subscribes to sway's `output` events (`swaymsg -t subscribe -m '["output"]'`)
//! and nudges the [`DisplayManager`] to reconcile whenever a display is
//! connected, disconnected, or reconfigured. Reconciliation is idempotent, so
//! the burst of events sway emits for a single hotplug collapses to at most one
//! arrangement change.

use crate::display::DisplayManager;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::watch;
use tracing::{info, warn};

/// Spawn the watcher task. It runs until `shutdown_rx` goes true, restarting the
/// `swaymsg` subscription if it exits unexpectedly.
pub fn spawn(
    manager: Arc<DisplayManager>,
    mut shutdown_rx: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if *shutdown_rx.borrow() {
                return;
            }
            let mut child = match tokio::process::Command::new("swaymsg")
                .args(["-t", "subscribe", "-m", r#"["output"]"#])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
            {
                Ok(c) => c,
                Err(e) => {
                    warn!(error = %e, "Failed to subscribe to sway output events; docking hotplug disabled");
                    return;
                }
            };
            let Some(stdout) = child.stdout.take() else {
                return;
            };
            let mut lines = BufReader::new(stdout).lines();
            info!("Watching sway output events for docking");

            loop {
                tokio::select! {
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            let _ = child.kill().await;
                            return;
                        }
                    }
                    line = lines.next_line() => {
                        match line {
                            // Each line is one output event (connect / disconnect
                            // / mode change). We don't inspect the payload —
                            // reconcile re-queries the full topology.
                            Ok(Some(_)) => manager.on_output_changed().await,
                            Ok(None) => break, // subscription ended; respawn
                            Err(e) => {
                                warn!(error = %e, "Error reading sway output events");
                                break;
                            }
                        }
                    }
                }
            }
            let _ = child.kill().await;
        }
    })
}
