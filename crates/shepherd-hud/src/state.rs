//! State management for the HUD
//!
//! The HUD subscribes to events from shepherdd and tracks session state.

use shepherd_api::{
    BrightnessInfo, BrightnessRestrictions, Event, EventPayload, InternetStatusView, VolumeInfo,
    VolumeRestrictions, WarningSeverity,
};
use shepherd_util::{EntryId, SessionId};
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
    /// default; shepherdd raises this for activities that disable sway's
    /// compositor scale so the HUD stays a normal physical size. See
    /// `EventPayload::HudScaleChanged`.
    scale_tx: Arc<watch::Sender<f64>>,
    scale_rx: watch::Receiver<f64>,
    /// Latest known status of each configured internet connectivity check.
    /// Empty when no checks are configured; in that case the HUD hides the
    /// indicator. Order matches the order in `ServiceStateSnapshot`.
    internet_tx: Arc<watch::Sender<Vec<InternetStatusView>>>,
    internet_rx: watch::Receiver<Vec<InternetStatusView>>,
}

impl SharedState {
    pub fn new() -> Self {
        let (session_tx, session_rx) = watch::channel(SessionState::NoSession);
        let (volume_tx, volume_rx) = watch::channel(None);
        let (brightness_tx, brightness_rx) = watch::channel(None);
        let (scale_tx, scale_rx) = watch::channel(1.0_f64);
        let (internet_tx, internet_rx) = watch::channel(Vec::new());

        Self {
            session_tx: Arc::new(session_tx),
            session_rx,
            volume_tx: Arc::new(volume_tx),
            volume_rx,
            brightness_tx: Arc::new(brightness_tx),
            brightness_rx,
            scale_tx: Arc::new(scale_tx),
            scale_rx,
            internet_tx: Arc::new(internet_tx),
            internet_rx,
        }
    }

    /// Current UI scale factor (1.0 = unmodified).
    pub fn scale_factor(&self) -> f64 {
        *self.scale_rx.borrow()
    }

    /// Current internet connectivity check status, in the order reported by
    /// shepherdd. Empty when no checks are configured.
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

    /// Update volume from VolumeChanged event (preserves restrictions from initial fetch)
    fn update_volume(&self, percent: u8, muted: bool) {
        self.volume_tx.send_modify(|vol| {
            if let Some(v) = vol {
                v.percent = percent;
                v.muted = muted;
            } else {
                // If we don't have initial volume yet, create a basic one
                *vol = Some(VolumeInfo {
                    percent,
                    muted,
                    available: true,
                    backend: None,
                    restrictions: VolumeRestrictions::unrestricted(),
                });
            }
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
    fn update_brightness(&self, percent: u8) {
        self.brightness_tx.send_modify(|br| {
            if let Some(b) = br {
                b.percent = percent;
            } else {
                *br = Some(BrightnessInfo {
                    percent,
                    available: true,
                    backend: None,
                    device: None,
                    restrictions: BrightnessRestrictions::unrestricted(),
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

    /// Handle an event from shepherdd
    pub fn handle_event(&self, event: &Event) {
        match &event.payload {
            EventPayload::SessionStarted {
                session_id,
                entry_id,
                label,
                deadline,
            } => {
                let now = shepherd_util::now();
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
                            };
                        }
                    }
                    // Handle update when already in Warning state (subsequent warnings)
                    else if let SessionState::Warning {
                        session_id: sid,
                        entry_id,
                        entry_name,
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
                self.set_internet_status(snapshot.internet_status.clone());
                if let Some(session) = &snapshot.current_session {
                    let now = shepherd_util::now();
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
                    });
                } else {
                    self.set_session_state(SessionState::NoSession);
                }
            }

            EventPayload::VolumeChanged { percent, muted } => {
                self.update_volume(*percent, *muted);
            }

            EventPayload::BrightnessChanged { percent } => {
                self.update_brightness(*percent);
            }

            EventPayload::InternetStatusChanged { target, available } => {
                self.update_internet_target(target, *available);
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

            _ => {}
        }
    }
}

impl Default for SharedState {
    fn default() -> Self {
        Self::new()
    }
}
