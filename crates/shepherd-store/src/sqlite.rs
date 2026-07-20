//! SQLite-based store implementation

use chrono::{DateTime, Local, NaiveDate};
use rusqlite::{Connection, OptionalExtension, params};
use shepherd_api::DailyOverride;
use shepherd_util::{EntryId, LimitSubject};
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;
use tracing::{debug, warn};

use crate::{AuditEvent, StateSnapshot, Store, StoreResult, TokenState};

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

    /// Rename a legacy `entry_id` key column to `subject` (issue #5).
    ///
    /// Limits used to hang off entries alone, so `cooldowns` and
    /// `daily_overrides` were keyed by a bare entry ID. Groups made the key a
    /// [`LimitSubject`], whose string form for an entry *is* the bare entry ID —
    /// so every existing row is already valid and only the column name is
    /// stale. That makes this metadata-only: no rows are read or rewritten.
    ///
    /// A no-op when the table doesn't exist yet (fresh database) or has already
    /// been renamed, so it is safe to run on every startup.
    fn rename_legacy_key_column(conn: &Connection, table: &str) -> StoreResult<()> {
        let has_legacy_column: bool = conn
            .prepare(&format!("PRAGMA table_info({table})"))?
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(Result::ok)
            .any(|name| name == "entry_id");

        if has_legacy_column {
            conn.execute_batch(&format!(
                "ALTER TABLE {table} RENAME COLUMN entry_id TO subject"
            ))?;
            debug!(table, "Migrated legacy entry_id key column to subject");
        }

        Ok(())
    }

    /// Add a column to an existing table if it is missing.
    ///
    /// `CREATE TABLE IF NOT EXISTS` is a no-op against a table that already
    /// exists, so a column added after a table shipped has to be applied by
    /// hand. Guarded by `PRAGMA table_info`, so it is a no-op on a fresh or
    /// already-migrated database.
    fn add_missing_column(
        conn: &Connection,
        table: &str,
        column: &str,
        definition: &str,
    ) -> StoreResult<()> {
        let table_exists: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?",
                params![table],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !table_exists {
            return Ok(());
        }

        let has_column: bool = conn
            .prepare(&format!("PRAGMA table_info({table})"))?
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(Result::ok)
            .any(|name| name == column);

        if !has_column {
            conn.execute_batch(&format!(
                "ALTER TABLE {table} ADD COLUMN {column} {definition}"
            ))?;
            debug!(table, column, "Added missing column");
        }

        Ok(())
    }

    /// The raw `token_balances` row for a subject: balance, day, ratchet.
    fn read_token_row(
        conn: &Connection,
        subject: &LimitSubject,
    ) -> StoreResult<Option<(i64, String, bool)>> {
        Ok(conn
            .query_row(
                "SELECT balance_secs, updated_day, ratcheted FROM token_balances WHERE subject = ?",
                params![subject.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get::<_, i64>(2)? != 0)),
            )
            .optional()?)
    }

    /// A raw row as of `day_str`, applying the lazy midnight reset: a
    /// non-carrying row from an earlier day has expired, balance and ratchet
    /// alike.
    fn effective_token_state(
        row: Option<(i64, String, bool)>,
        day_str: &str,
        carry_over: bool,
    ) -> TokenState {
        match row {
            Some((_, updated_day, _)) if !carry_over && updated_day != day_str => {
                TokenState::default()
            }
            Some((secs, _, ratcheted)) => TokenState {
                balance: Duration::from_secs(secs.max(0) as u64),
                ratcheted,
            },
            None => TokenState::default(),
        }
    }

    fn write_token_row(
        conn: &Connection,
        subject: &LimitSubject,
        balance_secs: i64,
        day_str: &str,
        ratcheted: bool,
    ) -> StoreResult<()> {
        conn.execute(
            r#"
            INSERT INTO token_balances (subject, balance_secs, updated_day, ratcheted)
            VALUES (?, ?, ?, ?)
            ON CONFLICT(subject)
            DO UPDATE SET
                balance_secs = excluded.balance_secs,
                updated_day = excluded.updated_day,
                ratcheted = excluded.ratcheted
            "#,
            params![subject.to_string(), balance_secs, day_str, ratcheted as i64],
        )?;
        Ok(())
    }

    fn init_schema(&self) -> StoreResult<()> {
        let conn = self.conn.lock().unwrap();

        // Must run before the CREATEs below: `CREATE TABLE IF NOT EXISTS` is a
        // no-op against an existing legacy table, so the rename is the only
        // thing that brings it up to the current shape.
        Self::rename_legacy_key_column(&conn, "cooldowns")?;
        Self::rename_legacy_key_column(&conn, "daily_overrides")?;
        // `token_balances` shipped one commit earlier, keyed by a bare entry ID
        // like the two above. Left unmigrated, every `WHERE subject = ?` fails
        // and — because the engine treats a failed balance read as zero — every
        // token gate locks silently.
        Self::rename_legacy_key_column(&conn, "token_balances")?;
        // The ratchet flag was added after `token_balances` shipped.
        Self::add_missing_column(
            &conn,
            "token_balances",
            "ratcheted",
            "INTEGER NOT NULL DEFAULT 0",
        )?;

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

            -- Token balances (issue #8), keyed by limit subject. `updated_day`
            -- is the local date of the last mutation, so a non-carrying balance
            -- resets lazily at midnight. `ratcheted` records that the gate has
            -- already opened for this balance, so spending it part-way down
            -- doesn't strand the remainder below `minimum_seconds`.
            CREATE TABLE IF NOT EXISTS token_balances (
                subject TEXT PRIMARY KEY,
                balance_secs INTEGER NOT NULL DEFAULT 0,
                updated_day TEXT NOT NULL,
                ratcheted INTEGER NOT NULL DEFAULT 0
            );

            -- Cooldowns, keyed by limit subject
            CREATE TABLE IF NOT EXISTS cooldowns (
                subject TEXT PRIMARY KEY,
                until TEXT NOT NULL
            );

            -- State snapshot (single row)
            CREATE TABLE IF NOT EXISTS snapshot (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                snapshot_json TEXT NOT NULL
            );

            -- Daily overrides set by parents, keyed by limit subject so a whole
            -- group can be enabled or disabled for the day (issue #5)
            CREATE TABLE IF NOT EXISTS daily_overrides (
                subject TEXT NOT NULL,
                date TEXT NOT NULL,
                availability INTEGER,
                quota_delta_seconds INTEGER,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (subject, date)
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

    fn get_token_state(
        &self,
        subject: &LimitSubject,
        day: NaiveDate,
        carry_over: bool,
    ) -> StoreResult<TokenState> {
        let conn = self.conn.lock().unwrap();
        let day_str = day.format("%Y-%m-%d").to_string();

        let row = Self::read_token_row(&conn, subject)?;
        Ok(Self::effective_token_state(row, &day_str, carry_over))
    }

    fn adjust_token_balance(
        &self,
        subject: &LimitSubject,
        day: NaiveDate,
        carry_over: bool,
        delta_secs: i64,
    ) -> StoreResult<TokenState> {
        // Read-modify-write under one transaction so a concurrent adjustment
        // can't lose an update.
        let mut conn = self.conn.lock().unwrap();
        let day_str = day.format("%Y-%m-%d").to_string();
        let tx = conn.transaction()?;

        let row = Self::read_token_row(&tx, subject)?;
        let current = Self::effective_token_state(row, &day_str, carry_over);
        let updated = (current.balance.as_secs() as i64)
            .saturating_add(delta_secs)
            .max(0);
        // An empty balance is locked whatever it once held, so spending it all
        // the way down releases the ratchet too.
        let ratcheted = current.ratcheted && updated > 0;

        Self::write_token_row(&tx, subject, updated, &day_str, ratcheted)?;
        tx.commit()?;

        debug!(
            subject = %subject,
            day = %day_str,
            delta_secs,
            balance_secs = updated,
            ratcheted,
            "Token balance adjusted"
        );
        Ok(TokenState {
            balance: Duration::from_secs(updated as u64),
            ratcheted,
        })
    }

    fn set_token_ratchet(
        &self,
        subject: &LimitSubject,
        day: NaiveDate,
        carry_over: bool,
    ) -> StoreResult<()> {
        let mut conn = self.conn.lock().unwrap();
        let day_str = day.format("%Y-%m-%d").to_string();
        let tx = conn.transaction()?;

        let row = Self::read_token_row(&tx, subject)?;
        let current = Self::effective_token_state(row, &day_str, carry_over);
        if current.ratcheted || current.balance.is_zero() {
            return Ok(());
        }

        Self::write_token_row(
            &tx,
            subject,
            current.balance.as_secs() as i64,
            &day_str,
            true,
        )?;
        tx.commit()?;

        debug!(subject = %subject, day = %day_str, "Token gate ratcheted open");
        Ok(())
    }

    fn get_cooldown_until(&self, subject: &LimitSubject) -> StoreResult<Option<DateTime<Local>>> {
        let conn = self.conn.lock().unwrap();

        let until_str: Option<String> = conn
            .query_row(
                "SELECT until FROM cooldowns WHERE subject = ?",
                [subject.to_string()],
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

    fn set_cooldown_until(
        &self,
        subject: &LimitSubject,
        until: DateTime<Local>,
    ) -> StoreResult<()> {
        let conn = self.conn.lock().unwrap();

        conn.execute(
            r#"
            INSERT INTO cooldowns (subject, until)
            VALUES (?, ?)
            ON CONFLICT(subject)
            DO UPDATE SET until = excluded.until
            "#,
            params![subject.to_string(), until.to_rfc3339()],
        )?;

        debug!(subject = %subject, until = %until, "Cooldown set");
        Ok(())
    }

    fn clear_cooldown(&self, subject: &LimitSubject) -> StoreResult<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM cooldowns WHERE subject = ?",
            [subject.to_string()],
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
        subject: &LimitSubject,
        date: NaiveDate,
    ) -> StoreResult<Option<DailyOverride>> {
        let conn = self.conn.lock().unwrap();
        let date_str = date.format("%Y-%m-%d").to_string();

        let row: Option<(Option<i64>, Option<i64>, String, String)> = conn
            .query_row(
                "SELECT availability, quota_delta_seconds, created_at, updated_at \
                 FROM daily_overrides WHERE subject = ? AND date = ?",
                params![subject.to_string(), date_str],
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
                subject: subject.clone(),
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
        subject: &LimitSubject,
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
            INSERT INTO daily_overrides (subject, date, availability, quota_delta_seconds, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, ?)
            ON CONFLICT(subject, date) DO UPDATE SET
                availability = excluded.availability,
                quota_delta_seconds = excluded.quota_delta_seconds,
                updated_at = excluded.updated_at
            "#,
            params![subject.to_string(), date_str, avail_int, quota_delta_seconds, now_str, now_str],
        )?;

        let created_at_str: String = conn.query_row(
            "SELECT created_at FROM daily_overrides WHERE subject = ? AND date = ?",
            params![subject.to_string(), date_str],
            |row| row.get(0),
        )?;
        let created_at = DateTime::parse_from_rfc3339(&created_at_str)
            .map(|dt| dt.with_timezone(&Local))
            .unwrap_or_else(|_| shepherd_util::now());
        let updated_at = shepherd_util::now();

        debug!(subject = %subject, date = %date_str, "Daily override upserted");
        Ok(DailyOverride {
            subject: subject.clone(),
            date,
            availability,
            quota_delta_seconds,
            created_at,
            updated_at,
        })
    }

    fn clear_daily_override(&self, subject: &LimitSubject, date: NaiveDate) -> StoreResult<bool> {
        let conn = self.conn.lock().unwrap();
        let date_str = date.format("%Y-%m-%d").to_string();

        let count = conn.execute(
            "DELETE FROM daily_overrides WHERE subject = ? AND date = ?",
            params![subject.to_string(), date_str],
        )?;

        debug!(subject = %subject, date = %date_str, deleted = count > 0, "Daily override cleared");
        Ok(count > 0)
    }

    fn list_daily_overrides(&self, date: NaiveDate) -> StoreResult<Vec<DailyOverride>> {
        let conn = self.conn.lock().unwrap();
        let date_str = date.format("%Y-%m-%d").to_string();

        let mut stmt = conn.prepare(
            "SELECT subject, availability, quota_delta_seconds, created_at, updated_at \
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
            let (subject_str, avail, delta, created_at_str, updated_at_str) = row?;
            let created_at = DateTime::parse_from_rfc3339(&created_at_str)
                .map(|dt| dt.with_timezone(&Local))
                .unwrap_or_else(|_| shepherd_util::now());
            let updated_at = DateTime::parse_from_rfc3339(&updated_at_str)
                .map(|dt| dt.with_timezone(&Local))
                .unwrap_or_else(|_| shepherd_util::now());
            overrides.push(DailyOverride {
                subject: subject_str
                    .parse()
                    .expect("LimitSubject parsing is infallible"),
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
        let entry_id = LimitSubject::entry("minecraft");
        let today = shepherd_util::now().date_naive();

        // Initially zero
        assert_eq!(
            store
                .get_token_state(&entry_id, today, false)
                .unwrap()
                .balance,
            Duration::ZERO
        );

        // Earning accumulates, and the adjustment returns the new balance
        let balance = store
            .adjust_token_balance(&entry_id, today, false, 600)
            .unwrap();
        assert_eq!(balance.balance, Duration::from_secs(600));
        let balance = store
            .adjust_token_balance(&entry_id, today, false, 300)
            .unwrap();
        assert_eq!(balance.balance, Duration::from_secs(900));
        assert_eq!(
            store
                .get_token_state(&entry_id, today, false)
                .unwrap()
                .balance,
            Duration::from_secs(900)
        );

        // Spending more than is banked saturates at zero rather than going
        // negative, so an overrun can't leave a debt behind.
        let balance = store
            .adjust_token_balance(&entry_id, today, false, -5000)
            .unwrap();
        assert_eq!(balance.balance, Duration::ZERO);
        assert_eq!(
            store
                .get_token_state(&entry_id, today, false)
                .unwrap()
                .balance,
            Duration::ZERO
        );
    }

    #[test]
    fn test_token_balance_resets_at_midnight_unless_carried_over() {
        let store = SqliteStore::in_memory().unwrap();
        let entry_id = LimitSubject::entry("minecraft");
        let yesterday = shepherd_util::now().date_naive() - chrono::Duration::days(1);
        let today = shepherd_util::now().date_naive();

        store
            .adjust_token_balance(&entry_id, yesterday, false, 1800)
            .unwrap();

        // Read as of today: the balance expired at local midnight.
        assert_eq!(
            store
                .get_token_state(&entry_id, today, false)
                .unwrap()
                .balance,
            Duration::ZERO
        );
        // ...but it is still there for a carry-over entry.
        assert_eq!(
            store
                .get_token_state(&entry_id, today, true)
                .unwrap()
                .balance,
            Duration::from_secs(1800)
        );

        // Earning today starts from zero for a non-carrying balance rather than
        // stacking on yesterday's.
        let balance = store
            .adjust_token_balance(&entry_id, today, false, 600)
            .unwrap();
        assert_eq!(balance.balance, Duration::from_secs(600));
    }

    #[test]
    fn test_token_balance_carries_over_across_days() {
        let store = SqliteStore::in_memory().unwrap();
        let entry_id = LimitSubject::entry("minecraft");
        let yesterday = shepherd_util::now().date_naive() - chrono::Duration::days(1);
        let today = shepherd_util::now().date_naive();

        store
            .adjust_token_balance(&entry_id, yesterday, true, 1800)
            .unwrap();
        let balance = store
            .adjust_token_balance(&entry_id, today, true, 600)
            .unwrap();
        assert_eq!(balance.balance, Duration::from_secs(2400));
    }

    /// A database written before groups existed keys cooldowns and overrides by
    /// a bare `entry_id`. Opening it must rename the column and leave the rows
    /// readable — an entry's subject string *is* its bare ID, so no row is
    /// rewritten (issue #5).
    #[test]
    fn test_legacy_entry_keyed_tables_are_migrated_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.db");
        let until = shepherd_util::now() + chrono::Duration::hours(1);
        let today = shepherd_util::now().date_naive();

        // Build the pre-groups schema and seed it.
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                r#"
                CREATE TABLE cooldowns (
                    entry_id TEXT PRIMARY KEY,
                    until TEXT NOT NULL
                );
                CREATE TABLE daily_overrides (
                    entry_id TEXT NOT NULL,
                    date TEXT NOT NULL,
                    availability INTEGER,
                    quota_delta_seconds INTEGER,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    PRIMARY KEY (entry_id, date)
                );
                -- Shipped by the token system (issue #8) one commit before
                -- groups re-keyed it, and without the ratchet column.
                CREATE TABLE token_balances (
                    entry_id TEXT PRIMARY KEY,
                    balance_secs INTEGER NOT NULL DEFAULT 0,
                    updated_day TEXT NOT NULL
                );
                "#,
            )
            .unwrap();
            conn.execute(
                "INSERT INTO token_balances (entry_id, balance_secs, updated_day) \
                 VALUES ('game-1', 900, ?)",
                params![today.format("%Y-%m-%d").to_string()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO cooldowns (entry_id, until) VALUES ('game-1', ?)",
                params![until.to_rfc3339()],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO daily_overrides (entry_id, date, availability, quota_delta_seconds, \
                 created_at, updated_at) VALUES ('game-1', ?, 1, 300, ?, ?)",
                params![
                    today.format("%Y-%m-%d").to_string(),
                    until.to_rfc3339(),
                    until.to_rfc3339()
                ],
            )
            .unwrap();
        }

        let store = SqliteStore::open(&path).unwrap();
        let subject = LimitSubject::entry("game-1");

        // Pre-existing state survives the migration intact.
        assert!(
            store.get_cooldown_until(&subject).unwrap().is_some(),
            "the legacy cooldown should still be readable"
        );
        let ov = store
            .get_daily_override(&subject, today)
            .unwrap()
            .expect("the legacy override should still be readable");
        assert_eq!(ov.subject, subject);
        assert_eq!(ov.availability, Some(true));
        assert_eq!(ov.quota_delta_seconds, Some(300));

        // And the migrated database now takes group-keyed rows too.
        let games = LimitSubject::group("games");
        store.set_cooldown_until(&games, until).unwrap();
        assert!(store.get_cooldown_until(&games).unwrap().is_some());
        assert!(
            store.get_cooldown_until(&subject).unwrap().is_some(),
            "a group cooldown must not disturb an entry's"
        );

        // A banked balance survives too — left unmigrated, every read would
        // fail and the engine would report a silently locked gate.
        let state = store.get_token_state(&subject, today, false).unwrap();
        assert_eq!(
            state.balance,
            Duration::from_secs(900),
            "the legacy token balance should still be readable"
        );
        assert!(
            !state.ratcheted,
            "a balance from before the ratchet existed defaults to locked"
        );

        // Re-opening is a no-op rather than an error.
        drop(store);
        let reopened = SqliteStore::open(&path).unwrap();
        assert!(reopened.get_cooldown_until(&subject).unwrap().is_some());
        assert_eq!(
            reopened
                .get_token_state(&subject, today, false)
                .unwrap()
                .balance,
            Duration::from_secs(900)
        );
    }

    /// The ratchet is remembered alongside the balance and released when the
    /// balance is spent to zero (issue #8).
    #[test]
    fn test_token_ratchet_persists_until_the_balance_is_spent() {
        let store = SqliteStore::in_memory().unwrap();
        let subject = LimitSubject::entry("minecraft");
        let today = shepherd_util::now().date_naive();

        store
            .adjust_token_balance(&subject, today, false, 600)
            .unwrap();
        store.set_token_ratchet(&subject, today, false).unwrap();
        assert!(
            store
                .get_token_state(&subject, today, false)
                .unwrap()
                .ratcheted,
            "the ratchet should be remembered"
        );

        // A partial spend keeps it.
        let state = store
            .adjust_token_balance(&subject, today, false, -400)
            .unwrap();
        assert_eq!(state.balance, Duration::from_secs(200));
        assert!(state.ratcheted);

        // Spending it out releases it, so the threshold has to be crossed again.
        let state = store
            .adjust_token_balance(&subject, today, false, -200)
            .unwrap();
        assert!(state.balance.is_zero());
        assert!(!state.ratcheted);
        let state = store
            .adjust_token_balance(&subject, today, false, 100)
            .unwrap();
        assert!(
            !state.ratcheted,
            "re-earning must not silently re-open the gate"
        );
    }

    /// The ratchet expires with the balance it belongs to.
    #[test]
    fn test_token_ratchet_resets_at_midnight_unless_carried_over() {
        let store = SqliteStore::in_memory().unwrap();
        let subject = LimitSubject::entry("minecraft");
        let today = shepherd_util::now().date_naive();
        let yesterday = today - chrono::Duration::days(1);

        store
            .adjust_token_balance(&subject, yesterday, false, 600)
            .unwrap();
        store.set_token_ratchet(&subject, yesterday, false).unwrap();

        assert!(
            !store
                .get_token_state(&subject, today, false)
                .unwrap()
                .ratcheted,
            "a non-carrying ratchet should expire with its balance"
        );
        assert!(
            store
                .get_token_state(&subject, today, true)
                .unwrap()
                .ratcheted,
            "a carried-over balance keeps its ratchet"
        );
    }

    #[test]
    fn test_cooldowns() {
        let store = SqliteStore::in_memory().unwrap();
        let entry_id = LimitSubject::entry("game-1");

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
