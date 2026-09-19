//! State management for the HUD
//!
//! The HUD subscribes to events from lunchboxd and tracks session state.

use lunchbox_api::{
    AudioOutput, BrightnessInfo, BrightnessRestrictions, DisplayState, Event, EventPayload,
    HudOrientation, InternetStatusView, VolumeInfo, VolumeRestrictions, WarningSeverity,
};
use lunchbox_util::{EntryId, SessionId};
use std::sync::Arc;
use tokio::sync::watch;

/// The current state of the session as seen by the HUD
#[derive(Debug, Clone)]
pub enum SessionState {
    /// No active session - HUD should be hidden
    NoSession,

    /// Session is active
    Active {
        session_id: SessionId,
        entry_id: EntryId,
        entry_name: String,
        started_at: std::time::Instant,
        time_limit_secs: Option<u64>,
        #[allow(dead_code)]
        time_remaining_secs: Option<u64>,
        /// Whether the "X" button should confirm before ending this activity
        /// (issue #78).
        confirm_on_close: bool,
        /// Whether this activity offers the reset button (issue #125).
        can_reset: bool,
        /// Whether this activity offers the page-turn buttons (issue #160).
        can_turn_pages: bool,
    },

    /// Warning shown - time running low
    Warning {
        session_id: SessionId,
        entry_id: EntryId,
        entry_name: String,
        warning_issued_at: std::time::Instant,
        time_remaining_at_warning: u64,
        /// Optional custom message from configuration
        message: Option<String>,
        /// Severity level of the warning
        severity: WarningSeverity,
        /// Whether the "X" button should confirm before ending this activity
        /// (issue #78).
        confirm_on_close: bool,
        /// Whether this activity offers the reset button (issue #125).
        can_reset: bool,
        /// Whether this activity offers the page-turn buttons (issue #160).
        can_turn_pages: bool,
    },

    /// Session is ending
    Ending {
        session_id: SessionId,
        reason: String,
    },
}

impl SessionState {
    /// Check if the HUD should be visible
    /// The HUD is always visible - it shows session info when active,
    /// or a minimal bar when no session
    pub fn is_visible(&self) -> bool {
        // Always show the HUD
        true
    }

    /// Get the current session ID if any
    pub fn session_id(&self) -> Option<&SessionId> {
        match self {
            SessionState::NoSession => None,
            SessionState::Active { session_id, .. } => Some(session_id),
            SessionState::Warning { session_id, .. } => Some(session_id),
            SessionState::Ending { session_id, .. } => Some(session_id),
        }
    }

    /// Display name of the current activity, if any. Used to make the
    /// close-confirmation prompt name the activity being ended (issue #78).
    pub fn entry_name(&self) -> Option<&str> {
        match self {
            SessionState::Active { entry_name, .. } | SessionState::Warning { entry_name, .. } => {
                Some(entry_name)
            }
            SessionState::NoSession | SessionState::Ending { .. } => None,
        }
    }

    /// Whether ending the current activity via the HUD "X" button should be
    /// confirmed first (issue #78). Only meaningful while a session is active;
    /// returns `false` when there is no session to end.
    pub fn confirm_on_close(&self) -> bool {
        match self {
            SessionState::Active {
                confirm_on_close, ..
            }
            | SessionState::Warning {
                confirm_on_close, ..
            } => *confirm_on_close,
            SessionState::NoSession | SessionState::Ending { .. } => false,
        }
    }

    /// Whether the current activity can be reset in place, i.e. whether the
    /// HUD should show its reset button (issue #125). `false` when there is no
    /// session, so the button hides along with the rest of the session UI.
    pub fn can_reset(&self) -> bool {
        match self {
            SessionState::Active { can_reset, .. } | SessionState::Warning { can_reset, .. } => {
                *can_reset
            }
            SessionState::NoSession | SessionState::Ending { .. } => false,
        }
    }

    /// Whether the current activity is one the HUD turns pages for, i.e.
    /// whether to show the page-turn buttons (issue #160). `false` with no
    /// session, so they hide with the rest of the session UI.
    pub fn can_turn_pages(&self) -> bool {
        match self {
            SessionState::Active { can_turn_pages, .. }
            | SessionState::Warning { can_turn_pages, .. } => *can_turn_pages,
            SessionState::NoSession | SessionState::Ending { .. } => false,
        }
    }
}

/// Shared state for the HUD
#[derive(Clone)]
pub struct SharedState {
    /// Session state sender
    session_tx: Arc<watch::Sender<SessionState>>,
    /// Session state receiver
    session_rx: watch::Receiver<SessionState>,
    /// Volume info sender (updated via events, not polling)
    volume_tx: Arc<watch::Sender<Option<VolumeInfo>>>,
    /// Volume info receiver
    volume_rx: watch::Receiver<Option<VolumeInfo>>,
    /// Brightness info sender (updated via events, not polling)
    brightness_tx: Arc<watch::Sender<Option<BrightnessInfo>>>,
    /// Brightness info receiver
    brightness_rx: watch::Receiver<Option<BrightnessInfo>>,
    /// UI scale multiplier applied on top of the compositor scale. 1.0 by
    /// default; lunchboxd raises this for activities that disable sway's
    /// compositor scale so the HUD stays a normal physical size. See
    /// `EventPayload::HudScaleChanged`.
    scale_tx: Arc<watch::Sender<f64>>,
    scale_rx: watch::Receiver<f64>,
    /// Which screen edge the HUD should occupy (issue #171). Follows the
    /// global `[service.hud]` setting except while an activity with its own
    /// `hud_orientation` is running. Like the scale factor this only ever
    /// arrives on *change*, so the event loop also seeds it from
    /// `get_hud_orientation` on every connect.
    orientation_tx: Arc<watch::Sender<HudOrientation>>,
    orientation_rx: watch::Receiver<HudOrientation>,
    /// Latest known status of each configured internet connectivity check.
    /// Empty when no checks are configured; in that case the HUD hides the
    /// indicator. Order matches the order in `ServiceStateSnapshot`.
    internet_tx: Arc<watch::Sender<Vec<InternetStatusView>>>,
    internet_rx: watch::Receiver<Vec<InternetStatusView>>,
    /// True while the system is suspending / awaiting fresh state on resume.
    /// The HUD shows placeholders for time, battery, and network in this
    /// state so the frame frozen across the suspend/resume gap is never a
    /// stale status (issue #73). Cleared by the fresh `StateChanged` that
    /// lunchboxd broadcasts after the post-resume connectivity re-check.
    suspended_tx: Arc<watch::Sender<bool>>,
    suspended_rx: watch::Receiver<bool>,
    /// Latest external-display arrangement (issue #87). `None` until the first
    /// `DisplayModeChanged` (or initial `get_display_state`) arrives. Drives the
    /// HUD's mirror/external toggle visibility and the active-output anchor.
    display_tx: Arc<watch::Sender<Option<DisplayState>>>,
    display_rx: watch::Receiver<Option<DisplayState>>,
    /// Whether the device is in administrator mode (issue #154). Drives the
    /// HUD's lock button, which is the only thing on the device's own screen
    /// that says the mode is on.
    admin_tx: Arc<watch::Sender<bool>>,
    admin_rx: watch::Receiver<bool>,
    /// What the compositor is showing, polled while administrator mode is on so
    /// the taskbar has something to list (issue #154). Empty otherwise: the
    /// kiosk has nothing to switch between, and polling it would be pure cost.
    windows_tx: Arc<watch::Sender<Vec<lunchbox_api::WindowInfo>>>,
    windows_rx: watch::Receiver<Vec<lunchbox_api::WindowInfo>>,
}

impl SharedState {
    pub fn new() -> Self {
        let (session_tx, session_rx) = watch::channel(SessionState::NoSession);
        let (volume_tx, volume_rx) = watch::channel(None);
        let (brightness_tx, brightness_rx) = watch::channel(None);
        let (scale_tx, scale_rx) = watch::channel(1.0_f64);
        let (orientation_tx, orientation_rx) = watch::channel(HudOrientation::default());
        let (internet_tx, internet_rx) = watch::channel(Vec::new());
        let (suspended_tx, suspended_rx) = watch::channel(false);
        let (display_tx, display_rx) = watch::channel(None);
        let (admin_tx, admin_rx) = watch::channel(false);
        let (windows_tx, windows_rx) = watch::channel(Vec::new());

        Self {
            session_tx: Arc::new(session_tx),
            session_rx,
            volume_tx: Arc::new(volume_tx),
            volume_rx,
            brightness_tx: Arc::new(brightness_tx),
            brightness_rx,
            scale_tx: Arc::new(scale_tx),
            orientation_tx: Arc::new(orientation_tx),
            orientation_rx,
            scale_rx,
            internet_tx: Arc::new(internet_tx),
            internet_rx,
            suspended_tx: Arc::new(suspended_tx),
            suspended_rx,
            display_tx: Arc::new(display_tx),
            display_rx,
            admin_tx: Arc::new(admin_tx),
            admin_rx,
            windows_tx: Arc::new(windows_tx),
            windows_rx,
        }
    }

    /// Whether the device is in administrator mode (issue #154).
    pub fn admin_mode(&self) -> bool {
        *self.admin_rx.borrow()
    }

    pub fn set_admin_mode(&self, active: bool) {
        let _ = self.admin_tx.send(active);
    }

    /// The compositor's windows, as of the last poll.
    pub fn windows(&self) -> Vec<lunchbox_api::WindowInfo> {
        self.windows_rx.borrow().clone()
    }

    pub fn set_windows(&self, windows: Vec<lunchbox_api::WindowInfo>) {
        let _ = self.windows_tx.send(windows);
    }

    /// Current external-display arrangement, if known.
    pub fn display_state(&self) -> Option<DisplayState> {
        self.display_rx.borrow().clone()
    }

    /// Replace the cached display arrangement (from `DisplayModeChanged` or the
    /// initial `get_display_state` fetch).
    pub fn set_display_state(&self, state: DisplayState) {
        let _ = self.display_tx.send(Some(state));
    }

    /// Whether the HUD should currently display suspend placeholders for time,
    /// battery, and network instead of live (possibly stale) values.
    pub fn is_suspended(&self) -> bool {
        *self.suspended_rx.borrow()
    }

    /// Set the suspend-placeholder flag.
    fn set_suspended(&self, suspended: bool) {
        let _ = self.suspended_tx.send(suspended);
    }

    /// Current UI scale factor (1.0 = unmodified).
    pub fn scale_factor(&self) -> f64 {
        *self.scale_rx.borrow()
    }

    /// The screen edge the HUD should currently occupy.
    pub fn orientation(&self) -> HudOrientation {
        *self.orientation_rx.borrow()
    }

    /// Record a new HUD edge. Also used by the event loop to seed the value on
    /// connect, which is why it is not confined to the event match below.
    pub fn set_orientation(&self, orientation: HudOrientation) {
        let _ = self.orientation_tx.send(orientation);
    }

    /// Current internet connectivity check status, in the order reported by
    /// lunchboxd. Empty when no checks are configured.
    pub fn internet_status(&self) -> Vec<InternetStatusView> {
        self.internet_rx.borrow().clone()
    }

    /// Replace the full internet status list (called from `StateChanged`).
    fn set_internet_status(&self, status: Vec<InternetStatusView>) {
        let _ = self.internet_tx.send(status);
    }

    /// Update a single check's availability (called from
    /// `InternetStatusChanged`). Adds the target if we haven't seen it yet.
    fn update_internet_target(&self, target: &str, available: bool) {
        self.internet_tx.send_modify(|list| {
            if let Some(entry) = list.iter_mut().find(|e| e.target == target) {
                entry.available = available;
            } else {
                list.push(InternetStatusView {
                    target: target.to_string(),
                    available,
                });
            }
        });
    }

    /// Get the current session state
    pub fn session_state(&self) -> SessionState {
        self.session_rx.borrow().clone()
    }

    /// Subscribe to session state changes
    #[allow(dead_code)]
    pub fn subscribe_session(&self) -> watch::Receiver<SessionState> {
        self.session_rx.clone()
    }

    /// Update session state
    pub fn set_session_state(&self, state: SessionState) {
        let _ = self.session_tx.send(state);
    }

    /// Get current volume info (cached from events)
    pub fn volume_info(&self) -> Option<VolumeInfo> {
        self.volume_rx.borrow().clone()
    }

    /// Set initial volume info (called once on connect)
    pub fn set_initial_volume(&self, info: VolumeInfo) {
        let _ = self.volume_tx.send(Some(info));
    }

    /// Apply a `VolumeChanged` event.
    ///
    /// The event carries the whole snapshot, so this replaces rather than merges.
    /// It used to splice in only `percent`/`muted` and keep the restrictions from
    /// the initial fetch, which went wrong as soon as the active output could
    /// change underneath us: the slider would keep enforcing the previous
    /// output's limits (issue #124).
    fn update_volume(
        &self,
        percent: u8,
        muted: bool,
        restrictions: VolumeRestrictions,
        output: Option<AudioOutput>,
    ) {
        self.volume_tx.send_modify(|vol| {
            // `available`/`backend` are host capabilities, not per-reading state,
            // and the event does not carry them; keep what the initial fetch saw.
            let (available, backend) = vol
                .as_ref()
                .map(|v| (v.available, v.backend.clone()))
                .unwrap_or((true, None));
            *vol = Some(VolumeInfo {
                percent,
                muted,
                available,
                backend,
                restrictions,
                output,
            });
        });
    }

    /// Get current brightness info (cached from events)
    pub fn brightness_info(&self) -> Option<BrightnessInfo> {
        self.brightness_rx.borrow().clone()
    }

    /// Set initial brightness info (called once on connect)
    pub fn set_initial_brightness(&self, info: BrightnessInfo) {
        let _ = self.brightness_tx.send(Some(info));
    }

    /// Update brightness from BrightnessChanged event (preserves
    /// restrictions/backend/device from the initial fetch).
    fn update_brightness(&self, percent: u8, auto_enabled: bool) {
        self.brightness_tx.send_modify(|br| {
            if let Some(b) = br {
                b.percent = percent;
                b.auto_enabled = auto_enabled;
            } else {
                *br = Some(BrightnessInfo {
                    percent,
                    available: true,
                    backend: None,
                    device: None,
                    restrictions: BrightnessRestrictions::unrestricted(),
                    auto_available: auto_enabled,
                    auto_enabled,
                });
            }
        });
    }

    /// Update time remaining for current session
    #[allow(dead_code)]
    pub fn update_time_remaining(&self, remaining_secs: u64) {
        self.session_tx.send_modify(|state| {
            if let SessionState::Active {
                time_remaining_secs,
                ..
            } = state
            {
                *time_remaining_secs = Some(remaining_secs);
            }
        });
    }

    /// Handle an event from lunchboxd
    pub fn handle_event(&self, event: &Event) {
        match &event.payload {
            EventPayload::SessionStarted {
                session_id,
                entry_id,
                label,
                deadline,
                confirm_on_close,
                can_reset,
                can_turn_pages,
            } => {
                let now = lunchbox_util::now();
                // For unlimited sessions (deadline=None), time_remaining is None
                let time_remaining = deadline.and_then(|d| {
                    if d > now {
                        Some((d - now).num_seconds().max(0) as u64)
                    } else {
                        Some(0)
                    }
                });
                self.set_session_state(SessionState::Active {
                    session_id: session_id.clone(),
                    entry_id: entry_id.clone(),
                    entry_name: label.clone(),
                    started_at: std::time::Instant::now(),
                    time_limit_secs: time_remaining,
                    time_remaining_secs: time_remaining,
                    confirm_on_close: *confirm_on_close,
                    can_reset: *can_reset,
                    can_turn_pages: *can_turn_pages,
                });
            }

            EventPayload::SessionEnded { session_id, .. }
                if self.session_state().session_id() == Some(session_id) =>
            {
                self.set_session_state(SessionState::NoSession);
            }

            EventPayload::WarningIssued {
                session_id,
                time_remaining,
                message,
                severity,
                ..
            } => {
                self.session_tx.send_modify(|state| {
                    // Handle transition from Active state
                    if let SessionState::Active {
                        session_id: sid,
                        entry_id,
                        entry_name,
                        confirm_on_close,
                        can_reset,
                        can_turn_pages,
                        ..
                    } = state
                    {
                        if sid == session_id {
                            *state = SessionState::Warning {
                                session_id: session_id.clone(),
                                entry_id: entry_id.clone(),
                                entry_name: entry_name.clone(),
                                warning_issued_at: std::time::Instant::now(),
                                time_remaining_at_warning: time_remaining.as_secs(),
                                message: message.clone(),
                                severity: *severity,
                                confirm_on_close: *confirm_on_close,
                                can_reset: *can_reset,
                                can_turn_pages: *can_turn_pages,
                            };
                        }
                    }
                    // Handle update when already in Warning state (subsequent warnings)
                    else if let SessionState::Warning {
                        session_id: sid,
                        entry_id,
                        entry_name,
                        confirm_on_close,
                        can_reset,
                        can_turn_pages,
                        ..
                    } = state
                        && sid == session_id
                    {
                        *state = SessionState::Warning {
                            session_id: session_id.clone(),
                            entry_id: entry_id.clone(),
                            entry_name: entry_name.clone(),
                            warning_issued_at: std::time::Instant::now(),
                            time_remaining_at_warning: time_remaining.as_secs(),
                            message: message.clone(),
                            severity: *severity,
                            confirm_on_close: *confirm_on_close,
                            can_reset: *can_reset,
                            can_turn_pages: *can_turn_pages,
                        };
                    }
                });
            }

            EventPayload::SessionExpiring { session_id }
                if self.session_state().session_id() == Some(session_id) =>
            {
                self.set_session_state(SessionState::Ending {
                    session_id: session_id.clone(),
                    reason: "Time expired".to_string(),
                });
            }

            EventPayload::StateChanged(snapshot) => {
                self.set_admin_mode(snapshot.admin_mode);
                // Fresh state has arrived (on resume this follows the
                // post-suspend connectivity re-check), so drop the suspend
                // placeholders and show live values again.
                self.set_suspended(false);
                self.set_internet_status(snapshot.internet_status.clone());
                if let Some(session) = &snapshot.current_session {
                    // Teardown in flight: the activity is still up, but the
                    // child needs to see that their close registered rather
                    // than an unchanged bar (issue #136).
                    if session.state == lunchbox_api::SessionState::Stopping {
                        self.set_session_state(SessionState::Ending {
                            session_id: session.session_id.clone(),
                            reason: "Closing…".to_string(),
                        });
                        return;
                    }
                    let now = lunchbox_util::now();
                    // For unlimited sessions (deadline=None), time_remaining is None
                    let time_remaining = session.deadline.map(|d| {
                        if d > now {
                            (d - now).num_seconds().max(0) as u64
                        } else {
                            0
                        }
                    });
                    self.set_session_state(SessionState::Active {
                        session_id: session.session_id.clone(),
                        entry_id: session.entry_id.clone(),
                        entry_name: session.label.clone(),
                        started_at: std::time::Instant::now(),
                        time_limit_secs: time_remaining,
                        time_remaining_secs: time_remaining,
                        confirm_on_close: session.confirm_on_close,
                        can_reset: session.can_reset,
                        can_turn_pages: session.can_turn_pages,
                    });
                } else {
                    self.set_session_state(SessionState::NoSession);
                }
            }

            EventPayload::VolumeChanged {
                percent,
                muted,
                restrictions,
                output,
            } => {
                self.update_volume(*percent, *muted, restrictions.clone(), output.clone());
            }

            EventPayload::BrightnessChanged {
                percent,
                auto_enabled,
            } => {
                self.update_brightness(*percent, *auto_enabled);
            }

            EventPayload::InternetStatusChanged { target, available } => {
                self.update_internet_target(target, *available);
            }

            EventPayload::DisplayModeChanged { state } => {
                self.set_display_state(state.clone());
            }

            EventPayload::HudScaleChanged { factor } => {
                // Reject obviously-broken factors (e.g. 0 or negative from a
                // misconfigured output) so we don't render an invisible HUD.
                let clamped = if factor.is_finite() && *factor > 0.0 {
                    factor.clamp(0.5, 4.0)
                } else {
                    1.0
                };
                let _ = self.scale_tx.send(clamped);
            }

            EventPayload::HudOrientationChanged { orientation } => {
                self.set_orientation(*orientation);
            }

            EventPayload::SystemSuspending => {
                // About to lose the ability to draw; switch time/battery/
                // network to placeholders so the frozen frame isn't stale.
                self.set_suspended(true);
            }

            EventPayload::SystemResumed => {
                // Keep the placeholders up until the fresh StateChanged (which
                // follows the post-resume connectivity re-check) clears them.
            }

            _ => {}
        }
    }
}

impl Default for SharedState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn active(confirm_on_close: bool) -> SessionState {
        SessionState::Active {
            session_id: SessionId::new(),
            entry_id: EntryId::new("game"),
            entry_name: "Some Game".into(),
            started_at: std::time::Instant::now(),
            time_limit_secs: None,
            time_remaining_secs: None,
            confirm_on_close,
            can_reset: false,
            can_turn_pages: false,
        }
    }

    /// The page buttons follow the activity, not the HUD: they belong to a
    /// reading session and must be gone the moment there isn't one, or a
    /// stray press would send a page key to the launcher.
    #[test]
    fn page_buttons_belong_to_the_session() {
        assert!(!active(true).can_turn_pages());
        assert!(!SessionState::NoSession.can_turn_pages());
        assert!(
            !SessionState::Ending {
                session_id: SessionId::new(),
                reason: "done".into(),
            }
            .can_turn_pages()
        );

        let reading = SessionState::Active {
            session_id: SessionId::new(),
            entry_id: EntryId::new("book"),
            entry_name: "A Book".into(),
            started_at: std::time::Instant::now(),
            time_limit_secs: None,
            time_remaining_secs: None,
            confirm_on_close: true,
            can_reset: false,
            can_turn_pages: true,
        };
        assert!(reading.can_turn_pages());
    }

    #[test]
    fn confirm_on_close_reflects_active_session() {
        assert!(active(true).confirm_on_close());
        assert!(!active(false).confirm_on_close());
    }

    #[test]
    fn confirm_on_close_is_false_without_a_session_to_end() {
        // Nothing to confirm when idle or already ending, so the "X" button
        // never prompts in those states (issue #78).
        assert!(!SessionState::NoSession.confirm_on_close());
        assert!(
            !SessionState::Ending {
                session_id: SessionId::new(),
                reason: "Time expired".into(),
            }
            .confirm_on_close()
        );
    }

    #[test]
    fn entry_name_exposed_for_active_session() {
        assert_eq!(active(true).entry_name(), Some("Some Game"));
        assert_eq!(SessionState::NoSession.entry_name(), None);
    }
}
