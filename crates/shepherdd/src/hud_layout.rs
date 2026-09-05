//! HUD placement for issue #171.
//!
//! Which screen edge the HUD occupies is configured globally under
//! `[service.hud]` and, per activity, with `hud_orientation` on the entry. The
//! global setting applies while the launcher is up and for every activity that
//! says nothing; an activity that does say something gets its edge for the life
//! of its session, and the global setting comes back when the session ends.
//!
//! The HUD is one long-lived process started by the compositor, not something
//! respawned per session, so it has to be *told*: this broadcasts
//! `HudOrientationChanged` whenever the effective edge changes, and answers
//! `get_hud_orientation` for a HUD that connected late or reconnected
//! mid-session. That pairing is deliberate and mirrors
//! `HudScaleChanged`/`get_hud_scale` in [`crate::hidpi`] — the event alone is
//! not enough, which is what issue #118 established for the scale factor.

use async_trait::async_trait;
use shepherd_api::{Event, EventPayload, HudOrientation};
use shepherd_host_api::HudLayoutController;
use shepherd_ipc::IpcServer;
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast};
use tracing::info;

/// Tracks the effective HUD edge and announces changes to it.
pub struct HudLayout {
    /// The configured `[service.hud]` edge. Never changes at runtime.
    global: HudOrientation,
    /// The running activity's override, if it asked for one.
    override_: Mutex<Option<HudOrientation>>,
    /// IPC server, used to push `HudOrientationChanged` to subscribed shells
    /// (in practice, shepherd-hud).
    ipc: Arc<IpcServer>,
    /// Daemon-wide event broadcast channel, to fan the same event out to HTTP
    /// SSE subscribers.
    event_tx: broadcast::Sender<Event>,
}

impl HudLayout {
    pub fn new(
        global: HudOrientation,
        ipc: Arc<IpcServer>,
        event_tx: broadcast::Sender<Event>,
    ) -> Self {
        Self {
            global,
            override_: Mutex::new(None),
            ipc,
            event_tx,
        }
    }

    /// Swap the override and broadcast if the *effective* edge moved.
    ///
    /// Comparing effective edges rather than overrides is what keeps an
    /// activity that asks for the edge the device already uses from producing
    /// a spurious event — and a spurious event costs a full HUD rebuild.
    async fn set_override(&self, next: Option<HudOrientation>) {
        let mut guard = self.override_.lock().await;
        let before = guard.unwrap_or(self.global);
        let after = next.unwrap_or(self.global);
        *guard = next;
        drop(guard);

        if before == after {
            return;
        }
        info!(?before, ?after, "HUD orientation changed");
        let event = Event::new(EventPayload::HudOrientationChanged { orientation: after });
        self.ipc.broadcast_event(event.clone());
        let _ = self.event_tx.send(event);
    }
}

#[async_trait]
impl HudLayoutController for HudLayout {
    async fn apply(&self, orientation: Option<HudOrientation>) {
        self.set_override(orientation).await;
    }

    async fn restore(&self) {
        self.set_override(None).await;
    }

    async fn orientation(&self) -> HudOrientation {
        self.override_.lock().await.unwrap_or(self.global)
    }
}
