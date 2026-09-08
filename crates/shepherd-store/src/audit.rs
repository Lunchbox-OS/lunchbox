//! Audit event types

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};
use shepherd_api::SessionEndReason;
use shepherd_util::{EntryId, LimitSubject, SessionId};
use std::time::Duration;

/// Types of audit events
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuditEventType {
    /// Service started
    ServiceStarted,

    /// Service stopped
    ServiceStopped,

    /// Policy loaded/reloaded
    PolicyLoaded { entry_count: usize },

    /// Session started
    SessionStarted {
        session_id: SessionId,
        entry_id: EntryId,
        label: String,
        /// Deadline for session. None means unlimited.
        deadline: Option<DateTime<Local>>,
    },

    /// Warning issued
    WarningIssued {
        session_id: SessionId,
        threshold_seconds: u64,
    },

    /// Session ended
    SessionEnded {
        session_id: SessionId,
        entry_id: EntryId,
        reason: SessionEndReason,
        duration: Duration,
    },

    /// Banked time granted or revoked by a caregiver (issue #8)
    TokensAdjusted {
        subject: LimitSubject,
        delta_seconds: i64,
        /// Balance after the adjustment, including any `max_balance` clawback.
        balance: Duration,
    },

    /// Launch denied
    LaunchDenied {
        entry_id: EntryId,
        reasons: Vec<String>,
    },

    /// Session extended (admin action)
    SessionExtended {
        session_id: SessionId,
        extended_by: Duration,
        new_deadline: DateTime<Local>,
    },

    /// An activity outlived every kill and kept running after its session
    /// ended (issue #136). Recorded so a caregiver reviewing the log can see
    /// that supervision was lost, and for how long — `resolved` marks the
    /// entry where the reconciliation sweep finally got rid of it.
    ActivityEscaped {
        session_id: SessionId,
        pid: u32,
        command: String,
        resolved: bool,
    },

    /// Config reload requested
    ConfigReloaded { success: bool },

    /// An administrator replaced the whole policy file through the management
    /// API (issue #185).
    ///
    /// Distinct from `PolicyLoaded`, which the engine records every time it
    /// picks a policy up — including the reload this write triggers a moment
    /// later. This row is the one that says a *person* changed what the device
    /// allows, rather than that the daemon re-read a file.
    PolicyWritten { entry_count: usize },

    /// Client connected
    ClientConnected {
        client_id: String,
        role: String,
        uid: Option<u32>,
    },

    /// Client disconnected
    ClientDisconnected { client_id: String },

    /// A peer was refused at accept by the management socket's allow-list
    /// (issue #144).
    ///
    /// Recorded because a refusal is more interesting than an acceptance: it
    /// is something at this uid reaching for a control surface it is not part
    /// of. The cgroup is best-effort — it is read after the decision, and a
    /// peer that has already exited leaves none — and an activity's scope is
    /// named after its session id, so it usually names what went looking.
    ClientRejected {
        reason: String,
        peer_cgroup: Option<String>,
        peer_pid: Option<u32>,
    },
}

/// Full audit event with metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    /// Unique event ID
    pub id: i64,

    /// Event timestamp
    pub timestamp: DateTime<Local>,

    /// Event type and details
    pub event: AuditEventType,
}

impl AuditEvent {
    pub fn new(event: AuditEventType) -> Self {
        Self {
            id: 0, // Will be set by store
            timestamp: shepherd_util::now(),
            event,
        }
    }
}
