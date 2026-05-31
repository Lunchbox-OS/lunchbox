//! System event sources that trigger an immediate internet re-check.
//!
//! The [`InternetMonitor`](crate::internet::InternetMonitor) polls on a fixed
//! interval, which means connectivity status can lag reality by up to one
//! interval after the machine wakes from suspend or a network adapter changes
//! state. To close that gap we subscribe to two system D-Bus signals and nudge
//! the monitor to re-check immediately:
//!
//! - `org.freedesktop.login1.Manager.PrepareForSleep` (fires with `start =
//!   false` on resume).
//! - `org.freedesktop.NetworkManager.StateChanged` (fires on adapter /
//!   connectivity transitions).
//!
//! Both signals live on the system bus, which shepherdd can read from inside
//! the kiosk session. Missing D-Bus or NetworkManager is non-fatal: the watcher
//! logs and retries, and the periodic checks keep working regardless.

use std::time::Duration;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tracing::{debug, info, warn};

/// Why an immediate internet re-check was requested. Used for logging only.
#[derive(Debug, Clone, Copy)]
pub enum RecheckTrigger {
    /// The machine resumed from suspend/hibernate.
    ResumedFromSleep,
    /// A network adapter or overall connectivity state changed.
    NetworkChanged,
}

/// Delay before reconnecting to the system bus after an error.
const RECONNECT_DELAY: Duration = Duration::from_secs(5);

#[zbus::proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait LogindManager {
    /// Emitted with `start = true` just before sleep and `start = false`
    /// after the system resumes.
    #[zbus(signal)]
    fn prepare_for_sleep(&self, start: bool) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager"
)]
trait NetworkManager {
    /// Emitted whenever NetworkManager's overall state changes (e.g. an
    /// adapter connects or disconnects).
    #[zbus(signal)]
    fn state_changed(&self, state: u32) -> zbus::Result<()>;
}

/// Spawn a background task that watches logind and NetworkManager signals and
/// sends a [`RecheckTrigger`] on `tx` whenever connectivity should be
/// re-evaluated. The task reconnects with a fixed backoff on any error.
pub fn spawn_recheck_watchers(tx: mpsc::UnboundedSender<RecheckTrigger>) {
    tokio::spawn(async move {
        loop {
            if let Err(err) = watch_once(&tx).await {
                warn!(
                    error = %err,
                    "System event watcher disconnected; retrying after backoff"
                );
            }
            // If the receiver is gone (monitor stopped), there is no reason to
            // keep retrying.
            if tx.is_closed() {
                return;
            }
            tokio::time::sleep(RECONNECT_DELAY).await;
        }
    });
}

/// Connect to the system bus, subscribe to both signals, and forward triggers
/// until an error occurs (at which point the caller reconnects).
async fn watch_once(tx: &mpsc::UnboundedSender<RecheckTrigger>) -> zbus::Result<()> {
    let connection = zbus::Connection::system().await?;

    let logind = LogindManagerProxy::new(&connection).await?;
    let nm = NetworkManagerProxy::new(&connection).await?;

    let mut sleep_signals = logind.receive_prepare_for_sleep().await?;
    let mut nm_signals = nm.receive_state_changed().await?;

    info!("Watching logind and NetworkManager for internet re-check triggers");

    loop {
        tokio::select! {
            signal = sleep_signals.next() => {
                let Some(signal) = signal else { break };
                let args = signal.args()?;
                // Only the resume edge matters; ignore the pre-sleep edge.
                if !args.start {
                    debug!("Resumed from sleep; requesting internet re-check");
                    if tx.send(RecheckTrigger::ResumedFromSleep).is_err() {
                        return Ok(());
                    }
                }
            }
            signal = nm_signals.next() => {
                let Some(_signal) = signal else { break };
                debug!("NetworkManager state changed; requesting internet re-check");
                if tx.send(RecheckTrigger::NetworkChanged).is_err() {
                    return Ok(());
                }
            }
        }
    }

    // A stream ended (bus dropped); surface as an error so the caller retries.
    Err(zbus::Error::Failure(
        "system bus signal stream ended".into(),
    ))
}
