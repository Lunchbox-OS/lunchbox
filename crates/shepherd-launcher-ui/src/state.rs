//! Launcher application state management

use shepherd_api::{EntryView, Event, EventPayload, ServiceStateSnapshot};
use shepherd_util::SessionId;
use std::time::Duration;
use tokio::sync::watch;

/// Current state of the launcher UI
#[derive(Debug, Clone, Default)]
pub enum LauncherState {
    /// Not connected to shepherdd
    #[default]
    Disconnected,
    /// Connected, waiting for initial state
    Connecting,
    /// Connected, no session running - show grid
    Idle { entries: Vec<EntryView> },
    /// Launch requested, waiting for response
    Launching {
        #[allow(dead_code)]
        entry_id: String,
    },
    /// The activity is being torn down. Shown so the child gets feedback that
    /// their close registered — without it the screen looks unchanged for as
    /// long as teardown takes, which is what made them press again on
    /// 2026-08-20 (issue #136). Non-interactive: the grid is not reachable
    /// from here, so a second press cannot launch anything.
    Closing { entry_label: String },
    /// Session is running
    SessionActive {
        #[allow(dead_code)]
        session_id: SessionId,
        entry_label: String,
        #[allow(dead_code)]
        time_remaining: Option<Duration>,
    },
    /// A caregiver is setting the device up (issue #154).
    ///
    /// Its own state rather than an empty grid: administrator mode disables
    /// every entry, so `Idle` would render a screen with nothing on it at all —
    /// no reason, no reassurance for the child, and no reminder to the
    /// caregiver that the kiosk is still unlocked.
    AdminMode,

    /// A startup step that visibly disrupts the screen is running (issue #2):
    /// show the loading page over the grid until shepherdd reports it done.
    /// Today that is the Waydroid pre-boot, which holds the outputs at scale 1
    /// — the grid would otherwise sit there rendering physically smaller than
    /// normal for the duration.
    StartingUp,
    /// Error state
    Error { message: String },
    /// System is suspending: show a static cover so the frozen frame across
    /// the suspend/resume gap isn't stale. Held until the fresh `StateChanged`
    /// that shepherdd broadcasts on resume replaces it (issue #73).
    Suspending,
}

/// Shared state container
#[derive(Clone)]
pub struct SharedState {
    sender: watch::Sender<LauncherState>,
    receiver: watch::Receiver<LauncherState>,
}

impl SharedState {
    pub fn new() -> Self {
        let (sender, receiver) = watch::channel(LauncherState::default());
        Self { sender, receiver }
    }

    pub fn set(&self, state: LauncherState) {
        let _ = self.sender.send(state);
    }

    #[allow(dead_code)]
    pub fn get(&self) -> LauncherState {
        self.receiver.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<LauncherState> {
        self.receiver.clone()
    }

    /// Update state from shepherdd event
    pub fn handle_event(&self, event: Event) {
        tracing::info!(event = ?event.payload, "Received event from shepherdd");
        match event.payload {
            EventPayload::StateChanged(snapshot) => {
                tracing::info!(
                    has_session = snapshot.current_session.is_some(),
                    "Applying state snapshot"
                );
                self.apply_snapshot(snapshot);
            }
            EventPayload::SessionStarted {
                session_id,
                entry_id: _,
                label,
                deadline,
                confirm_on_close: _,
                can_reset: _,
                can_turn_pages: _,
                kind_tag: _,
            } => {
                tracing::info!(session_id = %session_id, label = %label, "Session started event");
                let now = shepherd_util::now();
                // For unlimited sessions (deadline=None), time_remaining is None
                let time_remaining = deadline.and_then(|d| {
                    if d > now {
                        (d - now).to_std().ok()
                    } else {
                        Some(Duration::ZERO)
                    }
                });
                self.set(LauncherState::SessionActive {
                    session_id,
                    entry_label: label,
                    time_remaining,
                });
            }
            EventPayload::SessionEnded {
                session_id,
                entry_id,
                reason,
                ..
            } => {
                tracing::info!(session_id = %session_id, entry_id = %entry_id, reason = ?reason, "Session ended event - setting Connecting");
                // Will be followed by StateChanged, but set to connecting
                // to ensure grid reloads
                self.set(LauncherState::Connecting);
            }
            EventPayload::SessionExpiring { .. } => {
                // Time's up indicator handled by HUD
            }
            EventPayload::WarningIssued { .. } => {
                // Warnings handled by HUD
            }
            EventPayload::PolicyReloaded { .. } => {
                // Request fresh state
                self.set(LauncherState::Connecting);
            }
            EventPayload::EntryAvailabilityChanged { .. } => {
                // Request fresh state
                self.set(LauncherState::Connecting);
            }
            EventPayload::SystemSuspending => {
                // Cover the screen before it freezes so the stale clock /
                // activity list isn't what's frozen on resume.
                tracing::info!("System suspending; showing cover");
                self.set(LauncherState::Suspending);
            }
            EventPayload::SystemResumed => {
                // Keep the cover up; the StateChanged shepherdd broadcasts on
                // resume replaces it with fresh content.
                tracing::info!("System resumed; awaiting fresh state");
            }
            EventPayload::Shutdown => {
                // Service is shutting down
                self.set(LauncherState::Disconnected);
            }
            EventPayload::AuditEntry { .. } => {
                // Audit events are for admin clients, ignore
            }
            EventPayload::DiagnosticsChanged(_) => {
                // Administrator-facing (issue #143); the child's launcher has
                // nothing to do with them. What the child *does* see is the
                // `ReasonCode` on a tile a diagnostic caused to be unavailable,
                // which arrives on the snapshot like every other reason.
            }
            EventPayload::VolumeChanged { .. } => {
                // Volume events are handled by HUD
            }
            EventPayload::BrightnessChanged { .. } => {
                // Brightness events are handled by HUD
            }
            EventPayload::HudScaleChanged { .. } | EventPayload::HudOrientationChanged { .. } => {
                // HUD-only events; ignored by the launcher.
            }
            EventPayload::InternetStatusChanged { .. } => {
                // The launcher receives entry availability updates via
                // EntryAvailabilityChanged / StateChanged; the raw check
                // status is HUD-only.
            }
            EventPayload::LockChanged { locked } => {
                // Nothing for the launcher to draw: a session lock covers every
                // surface, so whatever it is showing is already hidden by the
                // compositor. Logged because it explains a gap in the journal.
                tracing::info!(locked, "Screen lock changed");
            }
            EventPayload::AdminModeChanged { active } => {
                // The tiles are driven by the snapshot that follows this event:
                // every entry carries `ReasonCode::AdminMode` while the mode is
                // on, so the grid greys itself out without the launcher
                // tracking the mode. Logged because it explains a screenful of
                // suddenly-unavailable entries in the journal.
                tracing::info!(active, "Administrator mode changed");
            }
            EventPayload::DisplayModeChanged { .. } => {
                // External-display arrangement is handled by shepherdd and the
                // HUD; the launcher doesn't render it (issue #87).
            }
        }
    }

    /// Translate a `ServiceStateSnapshot` into the launcher's higher-level
    /// state. Every path that receives a snapshot — the initial `service_state`
    /// fetch, `StateChanged`, post-failure refreshes — goes through here, so a
    /// new snapshot field can't be honoured on one route and ignored on another
    /// (which is exactly how `startup_busy` first failed to show up).
    pub fn apply_snapshot(&self, snapshot: ServiceStateSnapshot) {
        // A live session wins: it is already covering the screen with the
        // activity, and a pre-boot overlapping one shouldn't pull the child back
        // to a loading page.
        if snapshot.current_session.is_none() && snapshot.startup_busy {
            tracing::info!("Startup step in progress; showing the loading page");
            self.set(LauncherState::StartingUp);
            return;
        }
        if let Some(session) = snapshot.current_session {
            if session.state == shepherd_api::SessionState::Stopping {
                self.set(LauncherState::Closing {
                    entry_label: session.label,
                });
                return;
            }
            let now = shepherd_util::now();
            // For unlimited sessions (deadline=None), time_remaining is None
            let time_remaining = session.deadline.and_then(|d| {
                if d > now {
                    (d - now).to_std().ok()
                } else {
                    Some(Duration::ZERO)
                }
            });
            self.set(LauncherState::SessionActive {
                session_id: session.session_id,
                entry_label: session.label,
                time_remaining,
            });
        } else if snapshot.admin_mode {
            // Checked after the session, not before: the two are mutually
            // exclusive in the engine, so if they ever disagree the running
            // activity is the more urgent truth to show.
            self.set(LauncherState::AdminMode);
        } else {
            self.set(LauncherState::Idle {
                entries: snapshot.entries,
            });
        }
    }
}

impl Default for SharedState {
    fn default() -> Self {
        Self::new()
    }
}
