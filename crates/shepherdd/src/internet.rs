//! Internet connectivity monitoring for shepherdd.

use shepherd_api::{Event, EventPayload};
use shepherd_config::{InternetCheckScheme, InternetCheckTarget, Policy};
use shepherd_core::CoreEngine;
use shepherd_ipc::IpcServer;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::{Mutex, broadcast, mpsc};
use tokio::time;
use tracing::{debug, warn};

use crate::system_events::RecheckTrigger;

pub struct InternetMonitor {
    targets: Vec<InternetCheckTarget>,
    interval: Duration,
    timeout: Duration,
}

impl InternetMonitor {
    pub fn from_policy(policy: &Policy) -> Option<Self> {
        let mut targets = Vec::new();

        if let Some(check) = policy.service.internet.check.clone() {
            targets.push(check);
        }

        for entry in &policy.entries {
            if entry.internet.required
                && let Some(check) = entry.internet.check.clone()
                && !targets.contains(&check)
            {
                targets.push(check);
            }
        }

        if targets.is_empty() {
            return None;
        }

        Some(Self {
            targets,
            interval: policy.service.internet.interval,
            timeout: policy.service.internet.timeout,
        })
    }

    pub async fn run(
        self,
        engine: Arc<Mutex<CoreEngine>>,
        ipc: Arc<IpcServer>,
        event_tx: broadcast::Sender<Event>,
        mut recheck_rx: mpsc::UnboundedReceiver<RecheckTrigger>,
    ) {
        // Initial check
        self.check_all(&engine, &ipc, &event_tx).await;

        let mut interval = time::interval(self.interval);
        // The first tick of a fresh interval is immediate; consume it so we
        // don't re-check right after the initial check above.
        interval.tick().await;
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    self.check_all(&engine, &ipc, &event_tx).await;
                }
                // A system event (resume from suspend, network adapter change)
                // asks us to re-check connectivity immediately.
                Some(trigger) = recheck_rx.recv() => {
                    // Coalesce a burst of triggers into a single re-check.
                    while recheck_rx.try_recv().is_ok() {}
                    debug!(?trigger, "Re-running internet checks due to system event");
                    self.check_all(&engine, &ipc, &event_tx).await;
                    // Restart the periodic cadence from this event.
                    interval.reset();
                }
                else => break,
            }
        }
    }

    async fn check_all(
        &self,
        engine: &Arc<Mutex<CoreEngine>>,
        ipc: &Arc<IpcServer>,
        event_tx: &broadcast::Sender<Event>,
    ) {
        for target in &self.targets {
            let available = check_target(target, self.timeout).await;
            let changed = {
                let mut eng = engine.lock().await;
                eng.set_internet_status(target.clone(), available)
            };

            if changed {
                debug!(
                    check = %target.original,
                    available,
                    "Internet connectivity status changed"
                );

                let event = Event::new(EventPayload::InternetStatusChanged {
                    target: target.original.clone(),
                    available,
                });
                ipc.broadcast_event(event.clone());
                let _ = event_tx.send(event);
            }
        }
    }
}

async fn check_target(target: &InternetCheckTarget, timeout: Duration) -> bool {
    match target.scheme {
        InternetCheckScheme::Tcp | InternetCheckScheme::Http | InternetCheckScheme::Https => {
            let connect = TcpStream::connect((target.host.as_str(), target.port));
            match time::timeout(timeout, connect).await {
                Ok(Ok(stream)) => {
                    drop(stream);
                    true
                }
                Ok(Err(err)) => {
                    debug!(
                        check = %target.original,
                        error = %err,
                        "Internet check failed"
                    );
                    false
                }
                Err(_) => {
                    warn!(check = %target.original, "Internet check timed out");
                    false
                }
            }
        }
    }
}
