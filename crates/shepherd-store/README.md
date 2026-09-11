# shepherd-store

Persistence layer for Shepherd.

## Overview

This crate provides durable storage for the Shepherd service, including:

- **Audit log** - Append-only record of all significant events
- **Usage accounting** - Track time used per entry per day
- **Cooldown tracking** - Remember when entries become available again
- **State snapshots** - Enable crash recovery

## Purpose

The store ensures:
- **Time accounting correctness** - Usage is recorded durably
- **Crash tolerance** - Service can resume after unexpected shutdown
- **Auditability** - All actions are logged for later inspection

## Backend

The primary implementation uses **SQLite** for reliability:

```rust
use shepherd_store::SqliteStore;

let store = SqliteStore::open("/var/lib/shepherdd/shepherdd.db")?;
```

SQLite provides:
- ACID transactions for usage accounting
- Automatic crash recovery via WAL mode
- Single-file database, easy to backup

## Store Trait

All storage operations go through the `Store` trait:

```rust
pub trait Store: Send + Sync {
    // Audit log
    fn append_audit(&self, event: AuditEvent) -> StoreResult<()>;
    fn get_recent_audits(&self, limit: usize) -> StoreResult<Vec<AuditEvent>>;

    // Usage accounting
    fn get_usage(&self, entry_id: &EntryId, day: NaiveDate) -> StoreResult<Duration>;
    fn add_usage(&self, entry_id: &EntryId, day: NaiveDate, duration: Duration) -> StoreResult<()>;

    // Token balances (issue #8), keyed by limit subject
    fn get_token_state(&self, subject: &LimitSubject, day: NaiveDate, carry_over: bool)
        -> StoreResult<TokenState>;
    fn adjust_token_balance(&self, subject: &LimitSubject, day: NaiveDate, carry_over: bool,
        delta_secs: i64) -> StoreResult<TokenState>;

    // Cooldown tracking, keyed by limit subject
    fn get_cooldown_until(&self, subject: &LimitSubject) -> StoreResult<Option<DateTime<Local>>>;
    fn set_cooldown_until(&self, subject: &LimitSubject, until: DateTime<Local>) -> StoreResult<()>;
    fn clear_cooldown(&self, subject: &LimitSubject) -> StoreResult<()>;

    // State snapshot
    fn load_snapshot(&self) -> StoreResult<Option<StateSnapshot>>;
    fn save_snapshot(&self, snapshot: &StateSnapshot) -> StoreResult<()>;

    // Health
    fn is_healthy(&self) -> bool;
}
```

## Usage

### Recording Session Usage

```rust
// When a session ends. The day is the one the session *started* on, not the
// one it ended on (issue #170) — see the engine's README for why.
let duration = session.actual_duration();
let day = session.started_at.date_naive();

store.add_usage(&entry_id, day, duration)?;
```

### Checking Quota Remaining

```rust
let today = Local::now().date_naive();
let used = store.get_usage(&entry_id, today)?;

if let Some(quota) = entry.limits.daily_quota {
    let remaining = quota.saturating_sub(used);
    if remaining.is_zero() {
        // Quota exhausted
    }
}
```

### Setting Cooldowns

```rust
use chrono::{Duration, Local};

// After session ends, set cooldown
let cooldown_until = Local::now() + Duration::minutes(10);
store.set_cooldown_until(&entry_id, cooldown_until)?;
```

### Checking Cooldown

```rust
if let Some(until) = store.get_cooldown_until(&entry_id)? {
    if until > Local::now() {
        // Still in cooldown
    }
}
```

## Audit Log

The audit log records significant events:

```rust
use shepherd_store::{AuditEvent, AuditEventType};

// Event types logged
store.append_audit(AuditEvent::new(AuditEventType::PolicyLoaded { entry_count: 5 }))?;
store.append_audit(AuditEvent::new(AuditEventType::SessionStarted { 
    session_id, 
    entry_id 
}))?;
store.append_audit(AuditEvent::new(AuditEventType::SessionEnded { 
    session_id, 
    reason: SessionEndReason::Expired 
}))?;
store.append_audit(AuditEvent::new(AuditEventType::WarningIssued { 
    session_id, 
    threshold_secs: 60 
}))?;
```

### Audit Event Types

- `PolicyLoaded` - Configuration loaded/reloaded
- `SessionStarted` - New session began
- `SessionEnded` - Session terminated (with reason)
- `WarningIssued` - Time warning shown to user
- `LaunchDenied` - Launch request rejected (with reasons)
- `ConfigReloaded` - Configuration hot-reloaded
- `ServiceStarted` - Service process started
- `ServiceStopped` - Service process stopped

## State Snapshots

For crash recovery, the service can save state snapshots:

```rust
use shepherd_store::{StateSnapshot, SessionSnapshot};

// Save current state
let snapshot = StateSnapshot {
    timestamp: Local::now(),
    active_session: Some(SessionSnapshot {
        session_id,
        entry_id,
        started_at,
        deadline,
        warnings_issued: vec![300, 60],
    }),
};
store.save_snapshot(&snapshot)?;

// On startup, check for unfinished session
if let Some(snapshot) = store.load_snapshot()? {
    if let Some(session) = snapshot.active_session {
        // Potentially recover or clean up
    }
}
```

## Database Schema

The SQLite store uses this schema:

```sql
-- Audit log (append-only)
CREATE TABLE audit_log (
    id INTEGER PRIMARY KEY,
    timestamp TEXT NOT NULL,
    event_type TEXT NOT NULL,
    event_data TEXT NOT NULL  -- JSON
);

-- Usage tracking (one row per entry per day)
CREATE TABLE usage (
    entry_id TEXT NOT NULL,
    day TEXT NOT NULL,  -- YYYY-MM-DD
    duration_secs INTEGER NOT NULL,
    PRIMARY KEY (entry_id, day)
);

-- Token balances (one row per gated subject). `updated_day` is the local date
-- of the last mutation, so a non-carrying balance resets lazily at midnight
-- rather than needing a sweep job. This is a live balance, not a per-day
-- ledger: callers pass the *current* day, never a past one, so the stamp can
-- never move backwards over a balance written since (issue #170). Whether the
-- gate is open is not stored: it follows from the balance and the policy's
-- `minimum_seconds` (issue #193), which only the engine knows.
CREATE TABLE token_balances (
    subject TEXT PRIMARY KEY,
    balance_secs INTEGER NOT NULL DEFAULT 0,
    updated_day TEXT NOT NULL  -- YYYY-MM-DD
);

-- Cooldown tracking
CREATE TABLE cooldowns (
    subject TEXT PRIMARY KEY,
    until TEXT NOT NULL  -- ISO 8601 timestamp
);

-- State snapshot (single row)
CREATE TABLE snapshot (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    data TEXT NOT NULL  -- JSON
);

-- Audio outputs seen, and any per-output volume limit (issue #124).
-- Rows are created by discovery: shepherdd records each output it observes so
-- the admin UI can list real devices for the parent to pick from. `kind` is the
-- advisory classification and is stored as its wire string; an unrecognised
-- value loads as `unknown` rather than failing the read.
CREATE TABLE audio_outputs (
    output_key TEXT PRIMARY KEY,  -- <device.name>:output:<route.name>
    description TEXT NOT NULL,
    kind TEXT NOT NULL,
    max_volume INTEGER,           -- NULL = no per-output cap
    min_volume INTEGER,
    last_seen TEXT NOT NULL
);
```

`record_audio_output_seen` deliberately does not write the limit columns: a
device disappearing and coming back must keep whatever cap the parent gave it.
Setting limits is a separate call, and it refuses an output that has never been
seen — the UI only offers keys it has listed, so an unknown key means a stale
client rather than a new device.

## Schema Migration

There is no migration framework: `init_schema` is a batch of
`CREATE TABLE IF NOT EXISTS`, which is a no-op against an existing table. Any
change to an existing table's shape must therefore be applied explicitly *before*
those statements run.

Two helpers do this, both guarded by a `PRAGMA table_info` check so they are
no-ops on a fresh or already-migrated database and safe to run on every startup:

- `rename_legacy_key_column` renames the `entry_id` key column of `cooldowns`,
  `daily_overrides` and `token_balances` to `subject` (issue #5). It is
  metadata-only: an entry's `LimitSubject` string form *is* its bare entry ID, so
  every pre-existing row is already valid and no data is read or rewritten.
- `drop_obsolete_column` drops a column nothing reads any more from a table that
  already exists — today, `token_balances.ratcheted`, retired when the token
  gate stopped staying open below its minimum (issue #193). An older binary
  opened against the migrated database adds it back with its default, so a
  downgrade is safe.

**Every table listed in a migration has to stay listed.** `token_balances` was
originally left out of the rename, and the failure mode is the argument for the
rule: the reads fail with "no such column", the engine treats a failed balance
read as zero, and every token gate locks with no error anywhere. Add the table to
the migration when you re-key it, not when someone reports it.

## Design Philosophy

- **Durability over performance** - Writes are synchronous by default
- **Simple queries** - No complex joins or aggregations needed at runtime
- **Append-only audit** - Never modify history
- **Portable format** - JSON for event data enables future migration

## Dependencies

- `rusqlite` - SQLite bindings
- `serde` / `serde_json` - Event serialization
- `chrono` - Timestamp handling
- `thiserror` - Error types
