# Overrides can bypass the daily time limit

> Issues: <https://git.armeafamily.com/albert/shepherd-launcher/issues>

## Prompt

> Make it so that overrides can bypass the daily time limit

(Invoked via the `/remote-control` workflow.)

## Background

A daily override (`DailyOverride`, set by a parent via
`PUT /api/v1/overrides/{entry_id}` or the "Enable Today" button in the web UI)
has two independent fields:

- `availability: Option<bool>` — `Some(true)` force-enables an entry for the
  day, `Some(false)` force-disables it.
- `quota_delta_seconds: Option<i64>` — a signed adjustment to the daily quota.

Before this change, a force-enable override (`availability = Some(true)`,
the `manually_enabled` flag in the engine) bypassed the **availability window**
but *not* the **daily quota**. A parent could enable an entry outside its normal
hours, yet the child would still hit `QuotaExhausted` once the day's usage was
spent — so the override couldn't actually grant extra play time without also
fiddling with `quota_delta_seconds`.

## Change

`crates/shepherd-core/src/engine.rs` — extend the existing `manually_enabled`
bypass (which already short-circuits `disabled`, the availability window, etc.)
to also skip the daily-quota cap, mirroring exactly how the window bypass works:

- `evaluate_entry()`: the `QuotaExhausted` check is now guarded by
  `!manually_enabled`, so a force-enabled entry is never disabled for an
  exhausted quota.
- `compute_max_duration()`: the daily-quota-remaining limit is likewise guarded
  by `!manually_enabled`, so the session is no longer capped by remaining quota.
  A bare per-session `max_run` still applies; only the daily quota is lifted.

`quota_delta_seconds` keeps working for the case where a parent wants to grant a
*finite* amount of extra time without a full bypass (it has no effect once
`availability = Some(true)`, since the cap is gone entirely).

## Semantics summary

| Override | Window | Daily quota |
|---|---|---|
| `availability = Some(true)` | bypassed | **bypassed (new)** |
| `availability = Some(false)` | n/a (disabled) | n/a (disabled) |
| `quota_delta_seconds = +N` | unchanged | quota raised by N |

## Tests

Added `test_enable_override_bypasses_daily_quota` in `engine.rs`: exhausts an
entry's daily quota, asserts it is `QuotaExhausted`/denied, sets an
`availability = true` override, then asserts the entry is enabled, has no
reasons, reports `max_run_if_started_now = None` (uncapped), and launches.

`cargo test -p shepherd-core`, `cargo clippy --all-targets -- -D warnings`, and
`cargo fmt --all` all pass.
