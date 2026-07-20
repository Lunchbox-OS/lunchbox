//! SQLite-based store implementation

use chrono::{DateTime, Local, NaiveDate};
use rusqlite::{Connection, OptionalExtension, params};
use shepherd_api::DailyOverride;
use shepherd_util::EntryId;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;
use tracing::{debug, warn};

use crate::{AuditEvent, StateSnapshot, Store, StoreResult};

/// SQLite-based store
pub struct SqliteStore {
    conn: Mutex<Connection>,
}

impl SqliteStore {
    /// Open or create a store at the given path
    pub fn open(path: impl AsRef<Path>) -> StoreResult<Self> {
        let conn = Connection::open(path)?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.init_schema()?;
        Ok(store)
    }

    /// Create an in-memory store (for testing)
    pub fn in_memory() -> StoreResult<Self> {
        let conn = Connection::open_in_memory()?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.init_schema()?;
        Ok(store)
    }

    fn init_schema(&self) -> StoreResult<()> {
        let conn = self.conn.lock().unwrap();

        conn.execute_batch(
            r#"
            -- Audit log (append-only)
            CREATE TABLE IF NOT EXISTS audit_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp TEXT NOT NULL,
                event_json TEXT NOT NULL
            );

            -- Usage accounting
            CREATE TABLE IF NOT EXISTS usage (
                entry_id TEXT NOT NULL,
                day TEXT NOT NULL,
                duration_secs INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (entry_id, day)
            );

            -- Token balances (issue #8). `updated_day` is the local date of the
            -- last mutation, so a non-carrying balance resets lazily at midnight.
            CREATE TABLE IF NOT EXISTS token_balances (
                entry_id TEXT PRIMARY KEY,
                balance_secs INTEGER NOT NULL DEFAULT 0,
                updated_day TEXT NOT NULL
            );

            -- Cooldowns
            CREATE TABLE IF NOT EXISTS cooldowns (
                entry_id TEXT PRIMARY KEY,
                until TEXT NOT NULL
            );

            -- State snapshot (single row)
            CREATE TABLE IF NOT EXISTS snapshot (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                snapshot_json TEXT NOT NULL
            );

            -- Daily overrides set by parents
            CREATE TABLE IF NOT EXISTS daily_overrides (
                entry_id TEXT NOT NULL,
                date TEXT NOT NULL,
                availability INTEGER,
                quota_delta_seconds INTEGER,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (entry_id, date)
            );

            -- Small global key/value settings (runtime-toggled flags)
            CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );

            -- Indexes
            CREATE INDEX IF NOT EXISTS idx_audit_timestamp ON audit_log(timestamp);
            CREATE INDEX IF NOT EXISTS idx_usage_day ON usage(day);
            CREATE INDEX IF NOT EXISTS idx_overrides_date ON daily_overrides(date);
            "#,
        )?;

        debug!("Store schema initialized");
        Ok(())
    }
}

impl Store for SqliteStore {
    fn append_audit(&self, mut event: AuditEvent) -> StoreResult<()> {
        let conn = self.conn.lock().unwrap();
        let event_json = serde_json::to_string(&event.event)?;

        conn.execute(
            "INSERT INTO audit_log (timestamp, event_json) VALUES (?, ?)",
            params![event.timestamp.to_rfc3339(), event_json],
        )?;

        event.id = conn.last_insert_rowid();
        debug!(event_id = event.id, "Audit event appended");

        Ok(())
    }

    fn get_recent_audits(&self, limit: usize) -> StoreResult<Vec<AuditEvent>> {
        let conn = self.conn.lock().unwrap();

        let mut stmt = conn
            .prepare("SELECT id, timestamp, event_json FROM audit_log ORDER BY id DESC LIMIT ?")?;

        let rows = stmt.query_map([limit], |row| {
            let id: i64 = row.get(0)?;
            let timestamp_str: String = row.get(1)?;
            let event_json: String = row.get(2)?;
            Ok((id, timestamp_str, event_json))
        })?;

        let mut events = Vec::new();
        for row in rows {
            let (id, timestamp_str, event_json) = row?;
            let timestamp = DateTime::parse_from_rfc3339(&timestamp_str)
                .map(|dt| dt.with_timezone(&Local))
                .unwrap_or_else(|_| shepherd_util::now());
            let event: crate::AuditEventType = serde_json::from_str(&event_json)?;

            events.push(AuditEvent {
                id,
                timestamp,
                event,
            });
        }

        Ok(events)
    }

    fn get_usage(&self, entry_id: &EntryId, day: NaiveDate) -> StoreResult<Duration> {
        let conn = self.conn.lock().unwrap();
        let day_str = day.format("%Y-%m-%d").to_string();

        let secs: Option<i64> = conn
            .query_row(
                "SELECT duration_secs FROM usage WHERE entry_id = ? AND day = ?",
                params![entry_id.as_str(), day_str],
                |row| row.get(0),
            )
            .optional()?;

        Ok(Duration::from_secs(secs.unwrap_or(0) as u64))
    }

    fn add_usage(&self, entry_id: &EntryId, day: NaiveDate, duration: Duration) -> StoreResult<()> {
        let conn = self.conn.lock().unwrap();
        let day_str = day.format("%Y-%m-%d").to_string();
        let secs = duration.as_secs() as i64;

        conn.execute(
            r#"
            INSERT INTO usage (entry_id, day, duration_secs)
            VALUES (?, ?, ?)
            ON CONFLICT(entry_id, day)
            DO UPDATE SET duration_secs = duration_secs + excluded.duration_secs
            "#,
            params![entry_id.as_str(), day_str, secs],
        )?;

        debug!(entry_id = %entry_id, day = %day_str, added_secs = secs, "Usage added");
        Ok(())
    }

    fn get_token_balance(
        &self,
        entry_id: &EntryId,
        day: NaiveDate,
        carry_over: bool,
    ) -> StoreResult<Duration> {
        let conn = self.conn.lock().unwrap();
        let day_str = day.format("%Y-%m-%d").to_string();

        let row: Option<(i64, String)> = conn
            .query_row(
                "SELECT balance_secs, updated_day FROM token_balances WHERE entry_id = ?",
                params![entry_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        let secs = match row {
            // A non-carrying balance from an earlier day has already expired.
            Some((_, updated_day)) if !carry_over && updated_day != day_str => 0,
            Some((secs, _)) => secs.max(0),
            None => 0,
        };

        Ok(Duration::from_secs(secs as u64))
    }

    fn adjust_token_balance(
        &self,
        entry_id: &EntryId,
        day: NaiveDate,
        carry_over: bool,
        delta_secs: i64,
    ) -> StoreResult<Duration> {
        // Read-modify-write under one transaction so a concurrent adjustment
        // can't lose an update.
        let mut conn = self.conn.lock().unwrap();
        let day_str = day.format("%Y-%m-%d").to_string();
        let tx = conn.transaction()?;

        let row: Option<(i64, String)> = tx
            .query_row(
                "SELECT balance_secs, updated_day FROM token_balances WHERE entry_id = ?",
                params![entry_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        let current = match row {
            Some((_, updated_day)) if !carry_over && updated_day != day_str => 0,
            Some((secs, _)) => secs.max(0),
            None => 0,
        };
        let updated = current.saturating_add(delta_secs).max(0);

        tx.execute(
            r#"
            INSERT INTO token_balances (entry_id, balance_secs, updated_day)
            VALUES (?, ?, ?)
            ON CONFLICT(entry_id)
            DO UPDATE SET balance_secs = excluded.balance_secs, updated_day = excluded.updated_day
            "#,
            params![entry_id.as_str(), updated, day_str],
        )?;
        tx.commit()?;

        debug!(
            entry_id = %entry_id,
            day = %day_str,
            delta_secs,
            balance_secs = updated,
            "Token balance adjusted"
        );
        Ok(Duration::from_secs(updated as u64))
    }

    fn get_cooldown_until(&self, entry_id: &EntryId) -> StoreResult<Option<DateTime<Local>>> {
        let conn = self.conn.lock().unwrap();

        let until_str: Option<String> = conn
            .query_row(
                "SELECT until FROM cooldowns WHERE entry_id = ?",
                [entry_id.as_str()],
                |row| row.get(0),
            )
            .optional()?;

        let result = until_str.and_then(|s| {
            DateTime::parse_from_rfc3339(&s)
                .map(|dt| dt.with_timezone(&Local))
                .ok()
        });

        Ok(result)
    }

    fn set_cooldown_until(&self, entry_id: &EntryId, until: DateTime<Local>) -> StoreResult<()> {
        let conn = self.conn.lock().unwrap();

        conn.execute(
            r#"
            INSERT INTO cooldowns (entry_id, until)
            VALUES (?, ?)
            ON CONFLICT(entry_id)
            DO UPDATE SET until = excluded.until
            "#,
            params![entry_id.as_str(), until.to_rfc3339()],
        )?;

        debug!(entry_id = %entry_id, until = %until, "Cooldown set");
        Ok(())
    }

    fn clear_cooldown(&self, entry_id: &EntryId) -> StoreResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM cooldowns WHERE entry_id = ?",
            [entry_id.as_str()],
        )?;
        Ok(())
    }

    fn load_snapshot(&self) -> StoreResult<Option<StateSnapshot>> {
        let conn = self.conn.lock().unwrap();

        let json: Option<String> = conn
            .query_row(
                "SELECT snapshot_json FROM snapshot WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .optional()?;

        match json {
            Some(s) => {
                let snapshot: StateSnapshot = serde_json::from_str(&s)?;
                Ok(Some(snapshot))
            }
            None => Ok(None),
        }
    }

    fn save_snapshot(&self, snapshot: &StateSnapshot) -> StoreResult<()> {
        let conn = self.conn.lock().unwrap();
        let json = serde_json::to_string(snapshot)?;

        conn.execute(
            r#"
            INSERT INTO snapshot (id, snapshot_json)
            VALUES (1, ?)
            ON CONFLICT(id)
            DO UPDATE SET snapshot_json = excluded.snapshot_json
            "#,
            [json],
        )?;

        debug!("Snapshot saved");
        Ok(())
    }

    fn get_setting(&self, key: &str) -> StoreResult<Option<String>> {
        let conn = self.conn.lock().unwrap();
        let value: Option<String> = conn
            .query_row("SELECT value FROM settings WHERE key = ?", [key], |row| {
                row.get(0)
            })
            .optional()?;
        Ok(value)
    }

    fn set_setting(&self, key: &str, value: &str) -> StoreResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            r#"
            INSERT INTO settings (key, value)
            VALUES (?, ?)
            ON CONFLICT(key)
            DO UPDATE SET value = excluded.value
            "#,
            [key, value],
        )?;
        Ok(())
    }

    fn is_healthy(&self) -> bool {
        match self.conn.lock() {
            Ok(conn) => conn.query_row("SELECT 1", [], |_| Ok(())).is_ok(),
            Err(_) => {
                warn!("Store lock poisoned");
                false
            }
        }
    }

    fn get_daily_override(
        &self,
        entry_id: &EntryId,
        date: NaiveDate,
    ) -> StoreResult<Option<DailyOverride>> {
        let conn = self.conn.lock().unwrap();
        let date_str = date.format("%Y-%m-%d").to_string();

        let row: Option<(Option<i64>, Option<i64>, String, String)> = conn
            .query_row(
                "SELECT availability, quota_delta_seconds, created_at, updated_at \
                 FROM daily_overrides WHERE entry_id = ? AND date = ?",
                params![entry_id.as_str(), date_str],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()?;

        Ok(row.map(|(avail, delta, created_at_str, updated_at_str)| {
            let created_at = DateTime::parse_from_rfc3339(&created_at_str)
                .map(|dt| dt.with_timezone(&Local))
                .unwrap_or_else(|_| shepherd_util::now());
            let updated_at = DateTime::parse_from_rfc3339(&updated_at_str)
                .map(|dt| dt.with_timezone(&Local))
                .unwrap_or_else(|_| shepherd_util::now());
            DailyOverride {
                entry_id: entry_id.clone(),
                date,
                availability: avail.map(|v| v != 0),
                quota_delta_seconds: delta,
                created_at,
                updated_at,
            }
        }))
    }

    fn upsert_daily_override(
        &self,
        entry_id: &EntryId,
        date: NaiveDate,
        availability: Option<bool>,
        quota_delta_seconds: Option<i64>,
    ) -> StoreResult<DailyOverride> {
        let conn = self.conn.lock().unwrap();
        let date_str = date.format("%Y-%m-%d").to_string();
        let now_str = shepherd_util::now().to_rfc3339();
        let avail_int: Option<i64> = availability.map(|b| if b { 1 } else { 0 });

        conn.execute(
            r#"
            INSERT INTO daily_overrides (entry_id, date, availability, quota_delta_seconds, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, ?)
            ON CONFLICT(entry_id, date) DO UPDATE SET
                availability = excluded.availability,
                quota_delta_seconds = excluded.quota_delta_seconds,
                updated_at = excluded.updated_at
            "#,
            params![entry_id.as_str(), date_str, avail_int, quota_delta_seconds, now_str, now_str],
        )?;

        let created_at_str: String = conn.query_row(
            "SELECT created_at FROM daily_overrides WHERE entry_id = ? AND date = ?",
            params![entry_id.as_str(), date_str],
            |row| row.get(0),
        )?;
        let created_at = DateTime::parse_from_rfc3339(&created_at_str)
            .map(|dt| dt.with_timezone(&Local))
            .unwrap_or_else(|_| shepherd_util::now());
        let updated_at = shepherd_util::now();

        debug!(entry_id = %entry_id, date = %date_str, "Daily override upserted");
        Ok(DailyOverride {
            entry_id: entry_id.clone(),
            date,
            availability,
            quota_delta_seconds,
            created_at,
            updated_at,
        })
    }

    fn clear_daily_override(&self, entry_id: &EntryId, date: NaiveDate) -> StoreResult<bool> {
        let conn = self.conn.lock().unwrap();
        let date_str = date.format("%Y-%m-%d").to_string();

        let count = conn.execute(
            "DELETE FROM daily_overrides WHERE entry_id = ? AND date = ?",
            params![entry_id.as_str(), date_str],
        )?;

        debug!(entry_id = %entry_id, date = %date_str, deleted = count > 0, "Daily override cleared");
        Ok(count > 0)
    }

    fn list_daily_overrides(&self, date: NaiveDate) -> StoreResult<Vec<DailyOverride>> {
        let conn = self.conn.lock().unwrap();
        let date_str = date.format("%Y-%m-%d").to_string();

        let mut stmt = conn.prepare(
            "SELECT entry_id, availability, quota_delta_seconds, created_at, updated_at \
             FROM daily_overrides WHERE date = ?",
        )?;

        let rows = stmt.query_map([date_str], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;

        let mut overrides = Vec::new();
        for row in rows {
            let (entry_id_str, avail, delta, created_at_str, updated_at_str) = row?;
            let created_at = DateTime::parse_from_rfc3339(&created_at_str)
                .map(|dt| dt.with_timezone(&Local))
                .unwrap_or_else(|_| shepherd_util::now());
            let updated_at = DateTime::parse_from_rfc3339(&updated_at_str)
                .map(|dt| dt.with_timezone(&Local))
                .unwrap_or_else(|_| shepherd_util::now());
            overrides.push(DailyOverride {
                entry_id: EntryId::new(entry_id_str),
                date,
                availability: avail.map(|v| v != 0),
                quota_delta_seconds: delta,
                created_at,
                updated_at,
            });
        }

        Ok(overrides)
    }

    fn get_usage_range(
        &self,
        entry_id: &EntryId,
        from: NaiveDate,
        to: NaiveDate,
    ) -> StoreResult<Vec<(NaiveDate, Duration)>> {
        let conn = self.conn.lock().unwrap();
        let from_str = from.format("%Y-%m-%d").to_string();
        let to_str = to.format("%Y-%m-%d").to_string();

        let mut stmt = conn.prepare(
            "SELECT day, duration_secs FROM usage \
             WHERE entry_id = ? AND day >= ? AND day <= ? \
             ORDER BY day ASC",
        )?;

        let rows = stmt.query_map(params![entry_id.as_str(), from_str, to_str], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;

        let mut results = Vec::new();
        for row in rows {
            let (day_str, secs) = row?;
            if let Ok(date) = NaiveDate::parse_from_str(&day_str, "%Y-%m-%d") {
                results.push((date, Duration::from_secs(secs as u64)));
            }
        }

        Ok(results)
    }

    fn get_all_usage_for_date(&self, date: NaiveDate) -> StoreResult<Vec<(EntryId, Duration)>> {
        let conn = self.conn.lock().unwrap();
        let date_str = date.format("%Y-%m-%d").to_string();

        let mut stmt = conn.prepare("SELECT entry_id, duration_secs FROM usage WHERE day = ?")?;

        let rows = stmt.query_map([date_str], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;

        let mut results = Vec::new();
        for row in rows {
            let (entry_id_str, secs) = row?;
            results.push((EntryId::new(entry_id_str), Duration::from_secs(secs as u64)));
        }

        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AuditEventType;

    #[test]
    fn test_in_memory_store() {
        let store = SqliteStore::in_memory().unwrap();
        assert!(store.is_healthy());
    }

    #[test]
    fn test_settings_roundtrip() {
        let store = SqliteStore::in_memory().unwrap();
        assert_eq!(store.get_setting("auto_brightness_enabled").unwrap(), None);
        store
            .set_setting("auto_brightness_enabled", "true")
            .unwrap();
        assert_eq!(
            store.get_setting("auto_brightness_enabled").unwrap(),
            Some("true".to_string())
        );
        // Upsert overwrites.
        store
            .set_setting("auto_brightness_enabled", "false")
            .unwrap();
        assert_eq!(
            store.get_setting("auto_brightness_enabled").unwrap(),
            Some("false".to_string())
        );
    }

    #[test]
    fn test_audit_log() {
        let store = SqliteStore::in_memory().unwrap();

        let event = AuditEvent::new(AuditEventType::ServiceStarted);
        store.append_audit(event).unwrap();

        let events = store.get_recent_audits(10).unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0].event, AuditEventType::ServiceStarted));
    }

    #[test]
    fn test_usage_accounting() {
        let store = SqliteStore::in_memory().unwrap();
        let entry_id = EntryId::new("game-1");
        let today = shepherd_util::now().date_naive();

        // Initially zero
        let usage = store.get_usage(&entry_id, today).unwrap();
        assert_eq!(usage, Duration::ZERO);

        // Add some usage
        store
            .add_usage(&entry_id, today, Duration::from_secs(300))
            .unwrap();
        let usage = store.get_usage(&entry_id, today).unwrap();
        assert_eq!(usage, Duration::from_secs(300));

        // Add more usage
        store
            .add_usage(&entry_id, today, Duration::from_secs(200))
            .unwrap();
        let usage = store.get_usage(&entry_id, today).unwrap();
        assert_eq!(usage, Duration::from_secs(500));
    }

    #[test]
    fn test_token_balance_accrues_and_saturates() {
        let store = SqliteStore::in_memory().unwrap();
        let entry_id = EntryId::new("minecraft");
        let today = shepherd_util::now().date_naive();

        // Initially zero
        assert_eq!(
            store.get_token_balance(&entry_id, today, false).unwrap(),
            Duration::ZERO
        );

        // Earning accumulates, and the adjustment returns the new balance
        let balance = store
            .adjust_token_balance(&entry_id, today, false, 600)
            .unwrap();
        assert_eq!(balance, Duration::from_secs(600));
        let balance = store
            .adjust_token_balance(&entry_id, today, false, 300)
            .unwrap();
        assert_eq!(balance, Duration::from_secs(900));
        assert_eq!(
            store.get_token_balance(&entry_id, today, false).unwrap(),
            Duration::from_secs(900)
        );

        // Spending more than is banked saturates at zero rather than going
        // negative, so an overrun can't leave a debt behind.
        let balance = store
            .adjust_token_balance(&entry_id, today, false, -5000)
            .unwrap();
        assert_eq!(balance, Duration::ZERO);
        assert_eq!(
            store.get_token_balance(&entry_id, today, false).unwrap(),
            Duration::ZERO
        );
    }

    #[test]
    fn test_token_balance_resets_at_midnight_unless_carried_over() {
        let store = SqliteStore::in_memory().unwrap();
        let entry_id = EntryId::new("minecraft");
        let yesterday = shepherd_util::now().date_naive() - chrono::Duration::days(1);
        let today = shepherd_util::now().date_naive();

        store
            .adjust_token_balance(&entry_id, yesterday, false, 1800)
            .unwrap();

        // Read as of today: the balance expired at local midnight.
        assert_eq!(
            store.get_token_balance(&entry_id, today, false).unwrap(),
            Duration::ZERO
        );
        // ...but it is still there for a carry-over entry.
        assert_eq!(
            store.get_token_balance(&entry_id, today, true).unwrap(),
            Duration::from_secs(1800)
        );

        // Earning today starts from zero for a non-carrying balance rather than
        // stacking on yesterday's.
        let balance = store
            .adjust_token_balance(&entry_id, today, false, 600)
            .unwrap();
        assert_eq!(balance, Duration::from_secs(600));
    }

    #[test]
    fn test_token_balance_carries_over_across_days() {
        let store = SqliteStore::in_memory().unwrap();
        let entry_id = EntryId::new("minecraft");
        let yesterday = shepherd_util::now().date_naive() - chrono::Duration::days(1);
        let today = shepherd_util::now().date_naive();

        store
            .adjust_token_balance(&entry_id, yesterday, true, 1800)
            .unwrap();
        let balance = store
            .adjust_token_balance(&entry_id, today, true, 600)
            .unwrap();
        assert_eq!(balance, Duration::from_secs(2400));
    }

    #[test]
    fn test_cooldowns() {
        let store = SqliteStore::in_memory().unwrap();
        let entry_id = EntryId::new("game-1");

        // No cooldown initially
        assert!(store.get_cooldown_until(&entry_id).unwrap().is_none());

        // Set cooldown
        let until = shepherd_util::now() + chrono::Duration::hours(1);
        store.set_cooldown_until(&entry_id, until).unwrap();

        let stored = store.get_cooldown_until(&entry_id).unwrap().unwrap();
        assert!((stored - until).num_seconds().abs() < 1);

        // Clear cooldown
        store.clear_cooldown(&entry_id).unwrap();
        assert!(store.get_cooldown_until(&entry_id).unwrap().is_none());
    }

    #[test]
    fn test_snapshot() {
        let store = SqliteStore::in_memory().unwrap();

        // No snapshot initially
        assert!(store.load_snapshot().unwrap().is_none());

        // Save snapshot
        let snapshot = StateSnapshot {
            timestamp: shepherd_util::now(),
            active_session: None,
        };
        store.save_snapshot(&snapshot).unwrap();

        // Load it back
        let loaded = store.load_snapshot().unwrap().unwrap();
        assert!(loaded.active_session.is_none());
    }
}
