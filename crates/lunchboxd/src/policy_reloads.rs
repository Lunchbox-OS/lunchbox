//! Telling a background task that the policy was reloaded.
//!
//! Several of lunchboxd's background tasks derive their settings from the
//! policy: which input devices to look for, which internet targets to probe
//! and how often. Building those settings once at boot and never again is how
//! a reload silently fails to reach them (issues #188, #236). A task that
//! waits on [`PolicyReloads::next`] alongside its own work re-reads the live
//! policy from the engine whenever it wakes, so the engine stays the only
//! place the policy lives.

use lunchbox_api::{Event, EventPayload};
use tokio::sync::broadcast;

/// The reloads announced on lunchboxd's event bus, and nothing else from it.
pub struct PolicyReloads {
    events: broadcast::Receiver<Event>,
}

impl PolicyReloads {
    /// Start listening. Only reloads announced after this call are seen, so
    /// subscribe before the task's first read of the policy, not after it.
    pub fn subscribe(event_tx: &broadcast::Sender<Event>) -> Self {
        Self {
            events: event_tx.subscribe(),
        }
    }

    /// Wait for the next reload. `None` once the bus has closed, which means
    /// the daemon is shutting down.
    ///
    /// Falling behind the bus counts as a reload: a missed event may have been
    /// one, and re-reading the policy costs a task far less than running on a
    /// stale one. Cancel-safe, so it can sit in a `select!`.
    pub async fn next(&mut self) -> Option<()> {
        loop {
            match self.events.recv().await {
                Ok(Event {
                    payload: EventPayload::PolicyReloaded { .. },
                    ..
                }) => return Some(()),
                Ok(_) => {}
                Err(broadcast::error::RecvError::Lagged(_)) => return Some(()),
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn wakes_on_a_reload_and_skips_everything_else() {
        let (tx, _) = broadcast::channel(8);
        let mut reloads = PolicyReloads::subscribe(&tx);
        tx.send(Event::new(EventPayload::LockChanged { locked: true }))
            .unwrap();
        tx.send(Event::new(EventPayload::PolicyReloaded { entry_count: 3 }))
            .unwrap();
        assert_eq!(reloads.next().await, Some(()));
    }

    #[tokio::test]
    async fn falling_behind_counts_as_a_reload() {
        let (tx, _) = broadcast::channel(1);
        let mut reloads = PolicyReloads::subscribe(&tx);
        for _ in 0..3 {
            tx.send(Event::new(EventPayload::LockChanged { locked: true }))
                .unwrap();
        }
        assert_eq!(reloads.next().await, Some(()));
    }

    #[tokio::test]
    async fn ends_when_the_bus_closes() {
        let (tx, _) = broadcast::channel::<Event>(1);
        let mut reloads = PolicyReloads::subscribe(&tx);
        drop(tx);
        assert_eq!(reloads.next().await, None);
    }
}
