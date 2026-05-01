# HTTP Management API

Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/24>

## Summary

Implemented a local/LAN-accessible HTTP REST API (`shepherd-http` crate) that allows a parent's phone or other client on the same network to manage shepherd without direct access to the launcher UI. Key capabilities:

- **Session overrides** — enable/disable activities for the day or adjust daily quotas
- **Runtime adjustments** — add/remove time from the active session, terminate sessions
- **Screen time analytics** — per-entry usage statistics over arbitrary date ranges
- **Config reload** — trigger hot-reload of the configuration file
- **Debug/maintenance mode** — temporarily bypass Sway compositor restrictions
- **Volume control** — read and set volume, respecting per-entry and global policy limits
- **Server-Sent Events** — real-time event stream (`GET /api/v1/events`) for live UIs

Authentication is optional Bearer token. Override logic lives in `CoreEngine` so both IPC and HTTP clients see the same effects.

## Design decisions

- **New crate `shepherd-http`** rather than embedding routes in `shepherdd`, to keep concerns separated and the daemon thin.
- **Override logic in CoreEngine** rather than the HTTP layer, so the IPC clients (HUD, launcher UI) also see daily override effects without duplication.
- **In-memory maintenance mode** (resets on restart) for safety — a crash or restart should not leave the compositor unlocked.
- **`reduce_current()` as a separate method** from `extend_current()` because the existing method only accepts `Duration` (unsigned). HTTP handler routes based on sign of `seconds` parameter.
- **`broadcast::channel`** shared between IPC and HTTP SSE subscribers. A `Self::broadcast()` helper in `shepherdd` calls both `ipc.broadcast_event()` and `tx.send()` to keep call-sites clean.
- **`daily_overrides` table** added to SQLite schema with `(entry_id, date)` primary key. `availability` (`NULL`/0/1) and `quota_delta_seconds` compose independently.
- **New `ReasonCode::ManuallyDisabled { until: NaiveDate }`** variant so the launcher UI can show a parent-set reason rather than a generic "unavailable" message.

## Key files

- `crates/shepherd-http/` — new crate (lib.rs, state.rs, auth.rs, error.rs, handlers/)
- `crates/shepherd-api/src/types.rs` — DailyOverride, UsageStat, MaintenanceState, ReasonCode::ManuallyDisabled
- `crates/shepherd-store/src/traits.rs` — 6 new Store trait methods
- `crates/shepherd-store/src/sqlite.rs` — daily_overrides table + implementations
- `crates/shepherd-config/src/schema.rs` — RawManagementApiConfig
- `crates/shepherd-config/src/policy.rs` — ManagementApiConfig
- `crates/shepherd-core/src/engine.rs` — override-aware evaluation, reduce_current(), compute_max_duration() with delta
- `crates/shepherd-host-api/src/traits.rs` — set_maintenance_mode() default no-op
- `crates/shepherdd/src/main.rs` — broadcast channel, HTTP server startup, Self::broadcast() helper
- `crates/shepherd-launcher-ui/src/client.rs` — ManuallyDisabled arm in reason_to_message()
- `config.example.toml` — [service.management_api] example section
- `Cargo.toml` — axum 0.8, tokio-stream workspace deps, shepherd-http member
