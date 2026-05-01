//! Store trait definitions

use chrono::{DateTime, Local, NaiveDate};
use shepherd_api::DailyOverride;
use shepherd_util::{EntryId, SessionId};
use std::time::Duration;

use crate::{AuditEvent, StoreResult};

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

    // Cooldown tracking

    /// Get cooldown expiry time for an entry
    fn get_cooldown_until(&self, entry_id: &EntryId) -> StoreResult<Option<DateTime<Local>>>;

    /// Set cooldown expiry time for an entry
    fn set_cooldown_until(&self, entry_id: &EntryId, until: DateTime<Local>) -> StoreResult<()>;

    /// Clear cooldown for an entry
    fn clear_cooldown(&self, entry_id: &EntryId) -> StoreResult<()>;

    // State snapshot

    /// Load last saved snapshot
    fn load_snapshot(&self) -> StoreResult<Option<StateSnapshot>>;

    /// Save state snapshot
    fn save_snapshot(&self, snapshot: &StateSnapshot) -> StoreResult<()>;

    // Health

    /// Check if store is healthy
    fn is_healthy(&self) -> bool;

    // Daily overrides

    /// Get the daily override for an entry on a given date, if any
    fn get_daily_override(
        &self,
        entry_id: &EntryId,
        date: NaiveDate,
    ) -> StoreResult<Option<DailyOverride>>;

    /// Upsert a daily override for an entry
    fn upsert_daily_override(
        &self,
        entry_id: &EntryId,
        date: NaiveDate,
        availability: Option<bool>,
        quota_delta_seconds: Option<i64>,
    ) -> StoreResult<DailyOverride>;

    /// Remove the daily override for an entry, returning true if one existed
    fn clear_daily_override(&self, entry_id: &EntryId, date: NaiveDate) -> StoreResult<bool>;

    /// List all daily overrides active on a given date
    fn list_daily_overrides(&self, date: NaiveDate) -> StoreResult<Vec<DailyOverride>>;

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
