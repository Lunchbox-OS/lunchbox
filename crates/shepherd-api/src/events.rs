//! Event types for shepherdd -> client streaming

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use shepherd_util::{EntryId, SessionId};
use std::time::Duration;

use crate::types::default_confirm_on_close;
use crate::{API_VERSION, ServiceStateSnapshot, SessionEndReason, WarningSeverity};

/// Event envelope
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Event {
    pub api_version: u32,
    pub timestamp: DateTime<Local>,
    pub payload: EventPayload,
}

impl Event {
    pub fn new(payload: EventPayload) -> Self {
        Self {
            api_version: API_VERSION,
            timestamp: shepherd_util::now(),
            payload,
        }
    }
}

/// All possible events from the service to clients
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventPayload {
    /// Full state snapshot (sent on subscribe and major changes)
    StateChanged(ServiceStateSnapshot),

    /// Session has started
    SessionStarted {
        session_id: SessionId,
        entry_id: EntryId,
        label: String,
        /// Deadline for session. None means unlimited.
        deadline: Option<DateTime<Local>>,
        /// Whether the HUD should confirm before its "X" button ends this
        /// session (issue #78). Defaults to `true` when absent.
        #[serde(default = "default_confirm_on_close")]
        confirm_on_close: bool,
    },

    /// Warning issued for current session
    WarningIssued {
        session_id: SessionId,
        threshold_seconds: u64,
        time_remaining: Duration,
        severity: WarningSeverity,
        message: Option<String>,
    },

    /// Session is expiring (termination initiated)
    SessionExpiring { session_id: SessionId },

    /// Session has ended
    SessionEnded {
        session_id: SessionId,
        entry_id: EntryId,
        reason: SessionEndReason,
        duration: Duration,
    },

    /// Policy was reloaded
    PolicyReloaded { entry_count: usize },

    /// Entry availability changed (for UI updates)
    EntryAvailabilityChanged { entry_id: EntryId, enabled: bool },

    /// Volume status changed
    VolumeChanged { percent: u8, muted: bool },

    /// Screen brightness changed. `auto_enabled` reports whether automatic
    /// (ambient-light) brightness is currently on, so subscribers can keep an
    /// auto/manual indicator in sync from the same event.
    BrightnessChanged { percent: u8, auto_enabled: bool },

    /// HUD UI scale factor changed. The HUD is expected to multiply its
    /// font/padding/height by `factor` on top of the compositor scale.
    ///
    /// Emitted by shepherdd when it temporarily drops the compositor's
    /// output scale to 1.0 for an XWayland activity that cannot render at
    /// the panel's native resolution otherwise; on entry start the factor
    /// is the captured pre-launch output scale, and on entry exit it
    /// returns to 1.0. Clients that don't care can ignore it.
    HudScaleChanged { factor: f64 },

    /// Internet connectivity check changed. `target` matches the
    /// `InternetStatusView::target` field in `ServiceStateSnapshot`.
    InternetStatusChanged { target: String, available: bool },

    /// The external-display arrangement changed (issue #87). Shells use this to
    /// show/hide their mirror/external toggle and to re-anchor their layer-shell
    /// surface to the currently active output. Emitted on boot, on hotplug, and
    /// on every mode toggle.
    DisplayModeChanged { state: crate::DisplayState },

    /// The system is about to suspend/sleep. Clients should immediately
    /// commit a static "cover" frame (e.g. a loading screen) so the image
    /// frozen on screen across the suspend/resume gap is not stale (old
    /// clock, battery, or activity list). shepherdd holds a logind delay
    /// inhibitor for a short grace period after emitting this so clients have
    /// time to draw before the screen freezes.
    SystemSuspending,

    /// The system has resumed from suspend/sleep. A fresh `StateChanged`
    /// follows immediately so clients can replace the cover with up-to-date
    /// content.
    SystemResumed,

    /// The set of administrator-facing conditions changed (issue #143): one
    /// was raised, cleared, or updated.
    ///
    /// Carries the whole set rather than a delta. The set is small and capped,
    /// and a client that missed an event would otherwise need reconciliation
    /// logic to work out what it now holds — for a payload this size that is
    /// cost with no benefit.
    DiagnosticsChanged(crate::DiagnosticSet),

    /// Service is shutting down
    Shutdown,

    /// Audit event (for admin clients)
    AuditEntry {
        event_type: String,
        details: serde_json::Value,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_serialization() {
        let event = Event::new(EventPayload::SessionStarted {
            session_id: SessionId::new(),
            entry_id: EntryId::new("game-1"),
            label: "Test Game".into(),
            deadline: Some(shepherd_util::now()),
            confirm_on_close: true,
        });

        let json = serde_json::to_string(&event).unwrap();
        let parsed: Event = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.api_version, API_VERSION);
        assert!(matches!(
            parsed.payload,
            EventPayload::SessionStarted { .. }
        ));
    }

    #[test]
    fn event_serialization_unlimited() {
        // Test with unlimited session (deadline=None)
        let event = Event::new(EventPayload::SessionStarted {
            session_id: SessionId::new(),
            entry_id: EntryId::new("game-1"),
            label: "Unlimited Game".into(),
            deadline: None,
            confirm_on_close: false,
        });

        let json = serde_json::to_string(&event).unwrap();
        let parsed: Event = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.api_version, API_VERSION);
        if let EventPayload::SessionStarted { deadline, .. } = parsed.payload {
            assert!(deadline.is_none());
        } else {
            panic!("Expected SessionStarted");
        }
    }
}
