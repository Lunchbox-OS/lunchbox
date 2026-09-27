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
use lunchbox_api::{Event, EventPayload, HudOrientation};
use lunchbox_host_api::HudLayoutController;
use lunchbox_ipc::IpcServer;
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast};
use tracing::info;

/// The two settings the effective HUD edge is decided from.
#[derive(Debug, Clone, Copy)]
struct Edges {
    /// The configured `[service.hud]` edge. Follows config reloads (issue
    /// #244).
    global: HudOrientation,
    /// The running activity's override, if it asked for one.
    override_: Option<HudOrientation>,
}

impl Edges {
    fn effective(self) -> HudOrientation {
        self.override_.unwrap_or(self.global)
    }
}

/// Tracks the effective HUD edge and announces changes to it.
pub struct HudLayout {
    edges: Mutex<Edges>,
    /// IPC server, used to push `HudOrientationChanged` to subscribed shells
    /// (in practice, lunchbox-hud).
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
            edges: Mutex::new(Edges {
                global,
                override_: None,
            }),
            ipc,
            event_tx,
        }
    }

    /// Take the `[service.hud]` edge from a reloaded config (issue #244).
    ///
    /// Moves the HUD at once when no activity has an edge of its own. When one
    /// does, the new setting waits for the session to end, the same way the
    /// old one would have.
    pub async fn set_global(&self, global: HudOrientation) {
        self.update(|edges| edges.global = global).await;
    }

    /// Swap the override and broadcast if the *effective* edge moved.
    async fn set_override(&self, next: Option<HudOrientation>) {
        self.update(|edges| edges.override_ = next).await;
    }

    /// Change the edges and broadcast if the *effective* edge moved.
    ///
    /// Comparing effective edges rather than settings is what keeps an
    /// activity that asks for the edge the device already uses, or a reload
    /// that leaves `[service.hud]` alone, from producing a spurious event —
    /// and a spurious event costs a full HUD rebuild.
    async fn update(&self, change: impl FnOnce(&mut Edges)) {
        let mut edges = self.edges.lock().await;
        let before = edges.effective();
        change(&mut edges);
        let after = edges.effective();
        drop(edges);

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
        self.edges.lock().await.effective()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout(global: HudOrientation) -> (HudLayout, broadcast::Receiver<Event>) {
        let ipc = Arc::new(IpcServer::new(
            "/nonexistent-dir-for-lunchbox-tests/ipc.sock",
        ));
        let (tx, rx) = broadcast::channel(16);
        (HudLayout::new(global, ipc, tx), rx)
    }

    fn announced(rx: &mut broadcast::Receiver<Event>) -> Vec<HudOrientation> {
        let mut out = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let EventPayload::HudOrientationChanged { orientation } = event.payload {
                out.push(orientation);
            }
        }
        out
    }

    #[tokio::test]
    async fn a_reloaded_global_edge_moves_the_hud_at_once() {
        let (layout, mut rx) = layout(HudOrientation::Top);
        layout.set_global(HudOrientation::Left).await;
        assert_eq!(layout.orientation().await, HudOrientation::Left);
        assert_eq!(announced(&mut rx), [HudOrientation::Left]);
    }

    #[tokio::test]
    async fn an_unchanged_global_edge_announces_nothing() {
        let (layout, mut rx) = layout(HudOrientation::Top);
        layout.set_global(HudOrientation::Top).await;
        assert!(announced(&mut rx).is_empty());
    }

    #[tokio::test]
    async fn a_reloaded_global_edge_waits_for_an_activity_with_its_own() {
        let (layout, mut rx) = layout(HudOrientation::Top);
        layout.apply(Some(HudOrientation::Bottom)).await;
        layout.set_global(HudOrientation::Left).await;
        assert_eq!(layout.orientation().await, HudOrientation::Bottom);

        layout.restore().await;
        assert_eq!(layout.orientation().await, HudOrientation::Left);
        assert_eq!(
            announced(&mut rx),
            [HudOrientation::Bottom, HudOrientation::Left]
        );
    }
}
