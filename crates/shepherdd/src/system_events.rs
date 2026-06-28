//! System event sources from logind and NetworkManager.
//!
//! This watcher serves two purposes, both driven off the same system D-Bus
//! signals:
//!
//! 1. **Suspend cover (issue #73).** On suspend/resume the compositor cannot
//!    draw for seconds, leaving a stale frame (old clock, battery, activity
//!    list) frozen on screen. We listen for logind's `PrepareForSleep` and
//!    broadcast [`EventPayload::SystemSuspending`] / [`EventPayload::SystemResumed`]
//!    so clients can cover the screen with a static frame across the gap. To
//!    guarantee that cover frame is actually committed *before* the screen
//!    freezes, we hold a logind **delay** inhibitor and only release it after a
//!    short grace period once `SystemSuspending` has been emitted.
//!
//! 2. **Internet re-check.** The [`InternetMonitor`](crate::internet::InternetMonitor)
//!    polls on a fixed interval, so connectivity status can lag reality after a
//!    resume or an adapter change. When an internet monitor is configured we
//!    nudge it to re-check immediately on those events.
//!
//! Signals watched (both on the system bus, which shepherdd can read from
//! inside the kiosk session):
//!
//! - `org.freedesktop.login1.Manager.PrepareForSleep` (fires with `start =
//!   true` just before sleep and `start = false` on resume).
//! - `org.freedesktop.NetworkManager.StateChanged` (fires on adapter /
//!   connectivity transitions).
//!
//! Missing D-Bus / NetworkManager / logind is non-fatal: the watcher logs and
//! retries, and the periodic internet checks keep working regardless. If the
//! inhibitor cannot be acquired the cover events are still broadcast, just
//! without the guaranteed pre-sleep draw window.

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tracing::{debug, info, warn};

use shepherd_api::{Event, EventPayload};

/// Broadcasts an event to all subscribers (IPC + HTTP SSE). Supplied by the
/// service so this module does not need to know about either transport.
pub type BroadcastFn = Arc<dyn Fn(Event) + Send + Sync>;

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

/// How long shepherdd holds the logind delay inhibitor after emitting
/// `SystemSuspending`, giving clients time to draw the cover before the screen
/// freezes. Must stay well under logind's `InhibitDelayMaxSec` (5s default).
const SUSPEND_COVER_GRACE: Duration = Duration::from_millis(750);

#[zbus::proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait LogindManager {
    /// Take an inhibitor lock. With `mode = "delay"` logind defers the action
    /// (here: sleep) until the returned fd is closed or `InhibitDelayMaxSec`
    /// elapses, whichever comes first.
    #[zbus(name = "Inhibit")]
    fn inhibit(
        &self,
        what: &str,
        who: &str,
        why: &str,
        mode: &str,
    ) -> zbus::Result<zbus::zvariant::OwnedFd>;

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

/// Spawn a background task that watches logind and NetworkManager and:
///
/// - broadcasts suspend/resume cover events via `broadcast`,
/// - on resume, sends `()` on `resume_tx` so the service can push a fresh
///   `StateChanged` (clients drop the cover once up-to-date content arrives),
/// - on resume / network change, nudges the internet monitor through
///   `recheck_tx` if one is configured.
///
/// The task reconnects with a fixed backoff on any error and runs for the
/// lifetime of the service (the suspend cover is useful regardless of whether
/// internet gating is configured).
pub fn spawn_system_event_watchers(
    broadcast: BroadcastFn,
    recheck_tx: Option<mpsc::UnboundedSender<RecheckTrigger>>,
    resume_tx: mpsc::UnboundedSender<()>,
) {
    tokio::spawn(async move {
        loop {
            if let Err(err) = watch_once(&broadcast, &recheck_tx, &resume_tx).await {
                warn!(
                    error = %err,
                    "System event watcher disconnected; retrying after backoff"
                );
            }
            // The resume channel lives as long as the service main loop; once
            // it is gone shepherdd is shutting down and there is no reason to
            // keep retrying.
            if resume_tx.is_closed() {
                return;
            }
            tokio::time::sleep(RECONNECT_DELAY).await;
        }
    });
}

/// Acquire a logind delay inhibitor for sleep. Returns `None` (and logs) on
/// failure so the caller can carry on with best-effort cover broadcasts.
async fn acquire_sleep_inhibitor(
    logind: &LogindManagerProxy<'_>,
) -> Option<zbus::zvariant::OwnedFd> {
    match logind
        .inhibit(
            "sleep",
            "shepherdd",
            "Show suspend cover before sleep",
            "delay",
        )
        .await
    {
        Ok(fd) => {
            debug!("Acquired logind sleep delay inhibitor");
            Some(fd)
        }
        Err(err) => {
            warn!(
                error = %err,
                "Failed to acquire logind sleep inhibitor; suspend cover may not draw before sleep"
            );
            None
        }
    }
}

/// Connect to the system bus, subscribe to both signals, and handle events
/// until an error occurs (at which point the caller reconnects).
async fn watch_once(
    broadcast: &BroadcastFn,
    recheck_tx: &Option<mpsc::UnboundedSender<RecheckTrigger>>,
    resume_tx: &mpsc::UnboundedSender<()>,
) -> zbus::Result<()> {
    let connection = zbus::Connection::system().await?;

    let logind = LogindManagerProxy::new(&connection).await?;
    let nm = NetworkManagerProxy::new(&connection).await?;

    let mut sleep_signals = logind.receive_prepare_for_sleep().await?;
    let mut nm_signals = nm.receive_state_changed().await?;

    // Hold a delay inhibitor so we get a brief, guaranteed window to draw the
    // suspend cover before the screen freezes. Re-armed after each resume.
    let mut inhibitor = acquire_sleep_inhibitor(&logind).await;

    info!("Watching logind and NetworkManager for system events");

    loop {
        tokio::select! {
            signal = sleep_signals.next() => {
                let Some(signal) = signal else { break };
                let args = signal.args()?;
                if args.start {
                    // About to sleep: tell clients to show the cover, give them
                    // a moment to commit a frame, then release the inhibitor so
                    // the system proceeds to sleep.
                    debug!("Preparing for sleep; broadcasting SystemSuspending");
                    broadcast(Event::new(EventPayload::SystemSuspending));
                    tokio::time::sleep(SUSPEND_COVER_GRACE).await;
                    drop(inhibitor.take());
                } else {
                    // Resumed: announce it, then trigger a fresh StateChanged so
                    // clients drop their suspend cover/placeholders with current
                    // content. When an internet monitor is configured, let its
                    // resume re-check broadcast that StateChanged so connectivity
                    // is freshly probed (not the stale pre-suspend value the HUD
                    // would otherwise show). Otherwise ask the main loop to
                    // broadcast the snapshot directly. Finally re-arm the
                    // inhibitor for the next sleep.
                    debug!("Resumed from sleep; broadcasting SystemResumed");
                    broadcast(Event::new(EventPayload::SystemResumed));
                    if let Some(tx) = recheck_tx {
                        let _ = tx.send(RecheckTrigger::ResumedFromSleep);
                    } else {
                        let _ = resume_tx.send(());
                    }
                    if inhibitor.is_none() {
                        inhibitor = acquire_sleep_inhibitor(&logind).await;
                    }
                }
            }
            signal = nm_signals.next() => {
                let Some(_signal) = signal else { break };
                if let Some(tx) = recheck_tx {
                    debug!("NetworkManager state changed; requesting internet re-check");
                    let _ = tx.send(RecheckTrigger::NetworkChanged);
                }
            }
        }
    }

    // A stream ended (bus dropped); surface as an error so the caller retries.
    Err(zbus::Error::Failure(
        "system bus signal stream ended".into(),
    ))
}
