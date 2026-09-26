//! Internet connectivity monitoring for lunchboxd.

use lunchbox_api::{Event, EventPayload};
use lunchbox_config::{InternetCheckScheme, InternetCheckTarget, Policy};
use lunchbox_core::CoreEngine;
use lunchbox_ipc::IpcServer;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::{Mutex, broadcast, mpsc};
use tokio::time;
use tracing::{debug, info, warn};

use crate::policy_reloads::PolicyReloads;
use crate::system_events::RecheckTrigger;

/// What the monitor probes and how, as a policy asks for it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct InternetChecks {
    targets: Vec<InternetCheckTarget>,
    interval: Duration,
    timeout: Duration,
}

impl InternetChecks {
    fn from_policy(policy: &Policy) -> Self {
        Self {
            targets: policy
                .internet_check_targets()
                .into_iter()
                .cloned()
                .collect(),
            interval: policy.service.internet.interval,
            timeout: policy.service.internet.timeout,
        }
    }
}

/// Probes the policy's internet checks and feeds the answers to the engine.
///
/// Runs for the life of the daemon, even while the policy checks nothing, and
/// re-reads its targets, interval and timeout from the live policy on every
/// reload (issue #188). One built once from the boot policy never probed a
/// target a reload added, and the engine reports a target that has never been
/// probed as unreachable — so editing a check URL hid every activity gated on
/// it until the daemon restarted.
pub struct InternetMonitor {
    checks: InternetChecks,
}

impl InternetMonitor {
    pub fn from_policy(policy: &Policy) -> Self {
        Self {
            checks: InternetChecks::from_policy(policy),
        }
    }

    pub async fn run(
        mut self,
        engine: Arc<Mutex<CoreEngine>>,
        ipc: Arc<IpcServer>,
        event_tx: broadcast::Sender<Event>,
        mut recheck_rx: mpsc::UnboundedReceiver<RecheckTrigger>,
    ) {
        // Subscribed before the first check, so a reload that lands while it
        // runs is still seen.
        let mut reloads = PolicyReloads::subscribe(&event_tx);

        // Initial check
        self.check_all(&engine, &ipc, &event_tx).await;

        let mut interval = time::interval(self.checks.interval);
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
                    if self.checks.targets.is_empty() {
                        continue;
                    }
                    debug!(?trigger, "Re-running internet checks due to system event");
                    self.check_all(&engine, &ipc, &event_tx).await;
                    // Restart the periodic cadence from this event.
                    interval.reset();
                    // Broadcast a fresh full snapshot. `check_all` only emits
                    // InternetStatusChanged for targets that flipped, but on a
                    // resume re-check clients (the launcher cover and HUD
                    // suspend placeholders) need a StateChanged with the
                    // freshly-probed connectivity even when nothing changed.
                    let snapshot = engine.lock().await.get_state();
                    let event = Event::new(EventPayload::StateChanged(snapshot));
                    ipc.broadcast_event(event.clone());
                    let _ = event_tx.send(event);
                }
                reload = reloads.next() => match reload {
                    Some(()) => {
                        let checks = InternetChecks::from_policy(engine.lock().await.policy());
                        if checks == self.checks {
                            continue;
                        }
                        info!(
                            targets = ?checks.targets.iter().map(|t| &t.original).collect::<Vec<_>>(),
                            interval_secs = checks.interval.as_secs(),
                            "Internet checks changed; re-probing"
                        );
                        if checks.interval != self.checks.interval {
                            interval = time::interval(checks.interval);
                            interval.tick().await;
                        } else {
                            interval.reset();
                        }
                        self.checks = checks;
                        // Probe now rather than at the next tick: a target the
                        // reload added reads as unreachable until it is probed.
                        self.check_all(&engine, &ipc, &event_tx).await;
                    }
                    None => break,
                },
            }
        }
    }

    async fn check_all(
        &self,
        engine: &Arc<Mutex<CoreEngine>>,
        ipc: &Arc<IpcServer>,
        event_tx: &broadcast::Sender<Event>,
    ) {
        for target in &self.checks.targets {
            let available = check_target(target, self.checks.timeout).await;
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

#[cfg(test)]
mod tests {
    use super::*;
    use lunchbox_host_api::HostCapabilities;
    use lunchbox_store::SqliteStore;

    fn policy(internet: &str) -> Policy {
        lunchbox_config::parse_config(&format!(
            r#"
            config_version = 1
            {internet}

            [[entries]]
            id = "a"
            label = "A"
            kind = {{ type = "process", command = "/bin/a" }}
            "#
        ))
        .unwrap()
    }

    /// Issue #188: a check the boot policy did not have, added by reload, is
    /// probed — rather than reported unreachable for the life of the process.
    #[tokio::test]
    async fn a_check_added_by_reload_is_probed() {
        // Something reachable to probe: a bound listener completes a TCP
        // connect from its backlog without ever calling accept.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let boot = policy("");
        let store = Arc::new(SqliteStore::in_memory().unwrap());
        let engine = Arc::new(Mutex::new(CoreEngine::new(
            boot.clone(),
            store,
            HostCapabilities::minimal(),
        )));
        let ipc = Arc::new(IpcServer::new(
            "/nonexistent-dir-for-lunchbox-tests/ipc.sock",
        ));
        let (event_tx, _) = broadcast::channel(16);
        let (_recheck_tx, recheck_rx) = mpsc::unbounded_channel();

        let monitor = InternetMonitor::from_policy(&boot);
        tokio::spawn(monitor.run(engine.clone(), ipc, event_tx.clone(), recheck_rx));
        // The reload must not be announced before the monitor is listening.
        while event_tx.receiver_count() == 0 {
            tokio::task::yield_now().await;
        }

        let reloaded = policy(&format!(
            "[service.internet]\ncheck = \"tcp://127.0.0.1:{port}\"\ninterval_seconds = 3600"
        ));
        let entry_count = reloaded.entries.len();
        engine.lock().await.reload_policy(reloaded);
        event_tx
            .send(Event::new(EventPayload::PolicyReloaded { entry_count }))
            .unwrap();

        // Well inside the hour-long interval: only the reload can have probed it.
        time::timeout(Duration::from_secs(5), async {
            loop {
                let views = engine.lock().await.internet_status_views();
                if views.first().is_some_and(|view| view.available) {
                    break;
                }
                time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the check added by reload was never probed");
    }
}
