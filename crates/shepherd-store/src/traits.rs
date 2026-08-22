//! Store trait definitions

use chrono::{DateTime, Local, NaiveDate};
use shepherd_api::{AudioOutput, AudioOutputRecord, DailyOverride};
use shepherd_util::{EntryId, LimitSubject, SessionId};
use std::time::Duration;

use crate::{AuditEvent, StoreResult};

/// A subject's banked token state for a day (issue #8).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenState {
    /// Time banked and not yet spent.
    pub balance: Duration,
    /// Whether the gate has already opened for this balance. Once the balance
    /// has reached `minimum_seconds` the gate ratchets open and stays open
    /// until the balance is spent to zero, so a partial spend can't strand the
    /// remainder below the threshold.
    pub ratcheted: bool,
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
    ///
    /// A balance that reaches zero also clears the ratchet: an empty balance is
    /// locked whatever it once held.
    fn adjust_token_balance(
        &self,
        subject: &LimitSubject,
        day: NaiveDate,
        carry_over: bool,
        delta_secs: i64,
    ) -> StoreResult<TokenState>;

    /// Record that a subject's gate has been unlocked, so it stays unlocked
    /// while the balance lasts (issue #8).
    ///
    /// Only the engine knows the `minimum_seconds` threshold, so it decides
    /// when the ratchet catches; the store just remembers it alongside the
    /// balance, and resets it with the balance at local midnight.
    fn set_token_ratchet(
        &self,
        subject: &LimitSubject,
        day: NaiveDate,
        carry_over: bool,
    ) -> StoreResult<()>;

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

/// State snapshot for crash recovery
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StateSnapshot {
    /// Timestamp of snapshot
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
    pub deadline: DateTime<Local>,
    pub warnings_issued: Vec<u64>,
}
