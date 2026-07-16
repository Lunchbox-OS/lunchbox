//! XWayland HiDPI workaround for issue #45.
//!
//! Sway does not pass output scale through to XWayland clients, so an
//! XWayland game on a panel configured with `output * scale 1.5` only
//! renders at the logical resolution (e.g. 1280x720 on a 1080p panel)
//! and gets upscaled by sway, producing a blurry image. While an activity
//! with `xwayland_native_resolution = true` is running, shepherdd drops
//! every active sway output to scale 1.0 so the client sees the panel's
//! native pixel grid; on exit it restores the original per-output scales.
//!
//! The HUD is layer-shell and lives in sway's logical-pixel coordinate
//! space, so when sway drops to 1.0 the HUD would become physically smaller.
//! shepherdd broadcasts the captured pre-launch scale as a `HudScaleChanged`
//! event so the HUD can apply a counter-scale and stay readable.

use crate::display::DisplayManager;
use async_trait::async_trait;
use shepherd_api::{Event, EventPayload};
use shepherd_host_api::HidpiController;
use shepherd_host_linux::{OutputScale, get_outputs, set_output_scale};
use shepherd_ipc::IpcServer;
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast};
use tracing::{info, warn};

/// Manages the temporary scale override for an XWayland activity.
///
/// Implements [`HidpiController`] so the same instance can be threaded
/// into shepherd-http's `AppState` (where the IPC server and broadcast
/// channel aren't directly visible) and the IPC handlers in shepherdd.
pub struct XwaylandHidpi {
    /// Output scales captured at `apply` time so `restore` can reinstate
    /// them. `Some` means the workaround is currently active.
    saved: Mutex<Option<Vec<OutputScale>>>,
    /// IPC server, used to push `HudScaleChanged` to subscribed shells
    /// (notably shepherd-hud).
    ipc: Arc<IpcServer>,
    /// Daemon-wide event broadcast channel, used to fan the same event
    /// out to HTTP SSE subscribers.
    event_tx: broadcast::Sender<Event>,
    /// External-display controller (issue #87), if docking is enabled. After
    /// this workaround changes output scales for a native-resolution activity,
    /// it asks the controller to re-assert the mirror arrangement so a
    /// fullscreen activity can't leave the mirror mode / pin / pointer
    /// confinement broken. `None` when docking is disabled.
    display: Option<Arc<DisplayManager>>,
}

impl XwaylandHidpi {
    pub fn new(
        ipc: Arc<IpcServer>,
        event_tx: broadcast::Sender<Event>,
        display: Option<Arc<DisplayManager>>,
    ) -> Self {
        Self {
            saved: Mutex::new(None),
            ipc,
            event_tx,
            display,
        }
    }

    /// Re-assert the docking arrangement, if a controller is present. Called
    /// after scale changes so the mirror survives the activity.
    async fn reassert_display(&self) {
        if let Some(dm) = &self.display {
            dm.reassert().await;
        }
    }

    fn broadcast_factor(&self, factor: f64) {
        let event = Event::new(EventPayload::HudScaleChanged { factor });
        self.ipc.broadcast_event(event.clone());
        let _ = self.event_tx.send(event);
    }
}

#[async_trait]
impl HidpiController for XwaylandHidpi {
    /// Capture the current sway output scales, drop them to 1.0, and
    /// broadcast the captured scale as the HUD's compensating factor. Returns
    /// that factor so an Android launch can match it in density. No-op if already
    /// active (guards against re-entry), returning `1.0`.
    async fn apply(&self) -> f64 {
        let mut guard = self.saved.lock().await;
        if guard.is_some() {
            warn!("XWayland HiDPI workaround already active; refusing to re-apply");
            return 1.0;
        }
        let outputs = match get_outputs().await {
            Ok(o) => o,
            Err(e) => {
                warn!(error = %e, "Failed to query sway outputs; skipping XWayland HiDPI workaround");
                return 1.0;
            }
        };
        // Pick the largest scale among active outputs as the HUD factor.
        // For the common single-output kiosk this is just that output's
        // scale; multi-output errs on the readable side.
        let factor = outputs.iter().map(|o| o.scale).fold(1.0_f64, f64::max);
        if (factor - 1.0).abs() < f64::EPSILON && !outputs.is_empty() {
            // Nothing to counter-scale, so the HUD will render at its logical
            // size. That is correct on a genuinely 1x panel, and a symptom
            // otherwise: it also happens when a *previous* activity's restore
            // never ran (shepherdd restarted mid-session, or `set_output_scale`
            // failed), leaving sway at 1.0 with no record of the real scale.
            // Say so, because from the HUD's side the two are indistinguishable
            // and the second looks like "the HUD is too small" (issue #118).
            warn!(
                outputs = outputs.len(),
                "Every output is already at scale 1.0; the HUD will not counter-scale. \
                 If this panel is HiDPI, a previous session's scale restore was lost."
            );
        }
        for output in &outputs {
            if (output.scale - 1.0).abs() < f64::EPSILON {
                continue;
            }
            if let Err(e) = set_output_scale(&output.name, 1.0).await {
                warn!(name = %output.name, error = %e, "Failed to set output scale to 1.0");
            }
        }
        *guard = Some(outputs);
        info!(factor, "Applied XWayland HiDPI workaround");
        self.broadcast_factor(factor);
        drop(guard);
        // Re-assert the mirror after changing scales so the activity launches
        // into a correctly-configured docked layout (issue #87).
        self.reassert_display().await;
        factor
    }

    /// The factor a shell should currently be counter-scaling by: the largest
    /// captured pre-launch scale while the workaround is active, 1.0 otherwise.
    /// Derived from the same `saved` scales `restore` reinstates, so it cannot
    /// drift from what `apply` broadcast.
    async fn factor(&self) -> f64 {
        match self.saved.lock().await.as_ref() {
            Some(outputs) => outputs.iter().map(|o| o.scale).fold(1.0_f64, f64::max),
            None => 1.0,
        }
    }

    /// Restore the captured scales and broadcast factor=1.0. No-op if not
    /// active, so it is safe to call on every session end.
    async fn restore(&self) {
        let Some(outputs) = self.saved.lock().await.take() else {
            return;
        };
        // Tell the HUD to drop its compensating factor first: the brief
        // window where the HUD is at 1.0 and sway is also still at 1.0
        // is less jarring than the reverse (HUD compensating against a
        // scale that has already been restored, briefly oversized).
        self.broadcast_factor(1.0);
        for output in outputs {
            if let Err(e) = set_output_scale(&output.name, output.scale).await {
                warn!(name = %output.name, scale = output.scale, error = %e, "Failed to restore output scale");
            }
        }
        info!("Restored sway output scales");
        // Re-assert the mirror now that the activity has exited and scales are
        // back, so the mirror mode / pin / pointer are correct again (issue #87).
        self.reassert_display().await;
    }
}
