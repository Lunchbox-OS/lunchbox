//! Store trait definitions

use chrono::{DateTime, Local, NaiveDate};
use lunchbox_api::{AudioOutput, AudioOutputRecord, DailyOverride};
use lunchbox_util::{EntryId, LimitSubject, SessionId};
use std::time::Duration;

use crate::{AuditEvent, StoreResult};

/// A subject's banked token state for a day (issue #8).
///
/// `Serialize`/`Deserialize` because this crosses the wire to the state
/// custodian (issue #157); `Duration` and `bool` both have serde impls, so the
/// derive is enough and the representation stays the obvious one.
///
/// Whether the gate is open is deliberately not part of it: that follows from
/// the balance and `minimum_seconds` alone (issue #193), and only the engine
/// knows the threshold.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TokenState {
    /// Time banked and not yet spent.
    pub balance: Duration,
}

/// Main store trait
pub trait Store: Send + Sync {
    // Audit log

    /// Append an audit event
    fn append_audit(&self, event: AuditEvent) -> StoreResult<()>;

    /// Get recent audit events
    fn get_recent_audits(&self, limit: usize) -> StoreResult<Vec<AuditEvent>>;

    // Usage accounting

    /// Get total usage for an entry on a specific day
    fn get_usage(&self, entry_id: &EntryId, day: NaiveDate) -> StoreResult<Duration>;

    /// Add usage for an entry on a specific day
    fn add_usage(&self, entry_id: &EntryId, day: NaiveDate, duration: Duration) -> StoreResult<()>;

    // Token balances (issue #8)

    /// Get a subject's banked token state as of `day`.
    ///
    /// When `carry_over` is false and the balance was last touched on an
    /// earlier day, this returns the zero state — the balance resets lazily at
    /// local midnight rather than being swept by a job.
    fn get_token_state(
        &self,
        subject: &LimitSubject,
        day: NaiveDate,
        carry_over: bool,
    ) -> StoreResult<TokenState>;

    /// Apply a signed adjustment to a subject's token balance, returning the new
    /// state. Saturates at zero; `carry_over` has the same meaning as in
    /// [`Store::get_token_state`], so a non-carrying balance from an earlier
    /// day is treated as zero before the delta is applied.
    fn adjust_token_balance(
        &self,
        subject: &LimitSubject,
        day: NaiveDate,
        carry_over: bool,
        delta_secs: i64,
    ) -> StoreResult<TokenState>;

    // Cooldown tracking

    /// Get cooldown expiry time for a subject
    fn get_cooldown_until(&self, subject: &LimitSubject) -> StoreResult<Option<DateTime<Local>>>;

    /// Set cooldown expiry time for a subject
    fn set_cooldown_until(&self, subject: &LimitSubject, until: DateTime<Local>)
    -> StoreResult<()>;

    /// Clear cooldown for a subject
    fn clear_cooldown(&self, subject: &LimitSubject) -> StoreResult<()>;

    // State snapshot

    /// Load last saved snapshot
    fn load_snapshot(&self) -> StoreResult<Option<StateSnapshot>>;

    /// Save state snapshot
    fn save_snapshot(&self, snapshot: &StateSnapshot) -> StoreResult<()>;

    // Health

    /// Check if store is healthy
    fn is_healthy(&self) -> bool;

    // Daily overrides

    /// Get the daily override for a subject on a given date, if any
    fn get_daily_override(
        &self,
        subject: &LimitSubject,
        date: NaiveDate,
    ) -> StoreResult<Option<DailyOverride>>;

    /// Upsert a daily override for a subject
    fn upsert_daily_override(
        &self,
        subject: &LimitSubject,
        date: NaiveDate,
        availability: Option<bool>,
        quota_delta_seconds: Option<i64>,
    ) -> StoreResult<DailyOverride>;

    /// Remove the daily override for a subject, returning true if one existed
    fn clear_daily_override(&self, subject: &LimitSubject, date: NaiveDate) -> StoreResult<bool>;

    /// List all daily overrides active on a given date
    fn list_daily_overrides(&self, date: NaiveDate) -> StoreResult<Vec<DailyOverride>>;

    // Audio outputs (issue #124)

    /// Record that an output was observed, refreshing its label, kind, and
    /// `last_seen`. Never touches the limits: discovery and configuration are
    /// separate concerns, and a device reappearing must not reset its cap.
    fn record_audio_output_seen(&self, output: &AudioOutput) -> StoreResult<()>;

    /// Set (or clear, with `None`) the per-output limits for a known output.
    /// Returns false when no such row exists — the UI only offers keys it has
    /// listed, so an unknown key means a stale client rather than a new device.
    fn set_audio_output_limits(
        &self,
        output_key: &str,
        max_volume: Option<u8>,
        min_volume: Option<u8>,
    ) -> StoreResult<bool>;

    /// Fetch one output's record, if it has ever been seen.
    fn get_audio_output(&self, output_key: &str) -> StoreResult<Option<AudioOutputRecord>>;

    /// Every output ever seen, most recently seen first.
    fn list_audio_outputs(&self) -> StoreResult<Vec<AudioOutputRecord>>;

    /// Drop an output and its limits. Returns true if a row existed.
    fn forget_audio_output(&self, output_key: &str) -> StoreResult<bool>;

    // Settings (small, global, runtime-toggled key/value flags)

    /// Get a persisted setting by key, if present.
    fn get_setting(&self, key: &str) -> StoreResult<Option<String>>;

    /// Set (upsert) a persisted setting.
    fn set_setting(&self, key: &str, value: &str) -> StoreResult<()>;

    // Usage queries (extended)

    /// Get daily usage totals for an entry over a date range (inclusive)
    fn get_usage_range(
        &self,
        entry_id: &EntryId,
        from: NaiveDate,
        to: NaiveDate,
    ) -> StoreResult<Vec<(NaiveDate, Duration)>>;

    /// Get usage for all entries on a given date
    fn get_all_usage_for_date(&self, date: NaiveDate) -> StoreResult<Vec<(EntryId, Duration)>>;
}

/// State snapshot for crash recovery (issue #201).
///
/// Written while a session runs and cleared when it settles, so the presence of
/// an `active_session` at startup *is* the signal that the previous run never
/// got to settle one — a power cut, a crash, or a kill.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StateSnapshot {
    /// When this snapshot was taken. For a recovered session this is the last
    /// moment the daemon is known to have been alive, and so the end of what
    /// can honestly be charged.
    pub timestamp: DateTime<Local>,

    /// Active session info (if any)
    pub active_session: Option<SessionSnapshot>,
}

/// Snapshot of an active session
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionSnapshot {
    pub session_id: SessionId,
    pub entry_id: EntryId,
    pub started_at: DateTime<Local>,
    /// Wall-clock deadline. `None` for an unlimited session.
    pub deadline: Option<DateTime<Local>>,
    pub warnings_issued: Vec<u64>,

    /// What this session had billed as of [`StateSnapshot::timestamp`].
    ///
    /// Carried rather than re-derived, because the clock that measures it does
    /// not survive what this snapshot exists for. Billing runs on
    /// `CLOCK_MONOTONIC` — it excludes suspend (issue #155) and the spinner
    /// before the activity's window appears (issue #135) — and a reboot resets
    /// it. The wall clock survives but answers a different question, and the
    /// gap between the two is exactly the time a tamperer controls by choosing
    /// how long to leave the device off.
    pub billable: Duration,
}
