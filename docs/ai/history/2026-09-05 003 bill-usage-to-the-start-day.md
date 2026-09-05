# Billing a session to the day it started (issue #170)

> Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/170>

## Prompt

> implement #170

## Issue text

> **Time should be billed to the start day, not the end day**
>
> While activities crossing midnight is out of scope, we should still be billing
> usage to the start time, not the end time. Otherwise, "yesterday"'s usage end
> up costing the following day.

The defect was found while investigating #155 and split out there; see
[the #155 note](./2026-09-05%20002%20activity-clock-across-system-sleep.md),
"Related defects noticed in passing", which is also where the reporter agreed it
was real and out of scope for that issue.

## The defect

`CoreEngine::end_current_session` computed `let today = now.date_naive()` and
charged the whole session there. Usage is written once, at session end, so a
session from 23:50 to 00:10 put all twenty minutes on the **second** day — out
of a quota the child had not touched yet. An activity run right up to bedtime
therefore ate into the next morning.

`now.date_naive()` was also passed down through `settle_session_end` into
`settle_tokens`, so token settlement and the force-enable override lookup landed
on the same wrong day.

## What was implemented

Branch `fix/170-bill-usage-to-the-start-day`. One date, computed from the
session itself:

```rust
let billed_day = session.started_at.date_naive();
```

`ActiveSession::started_at` is the wall clock at launch approval and is never
mutated afterwards, so it is exactly the ledger key wanted. It flows to
`Store::add_usage` and into `settle_session_end` → `settle_tokens`.

Splitting a session across the two days it spans stays out of scope: the whole
session lands on its start day.

### Why the start of the session, not the start of billing

Billing runs from `window_ready_at_mono` (issue #135 — the child isn't charged
for the spinner), which is monotonic and has no wall-clock twin. Deriving one
would reintroduce exactly the drift #155 was about. `started_at` differs from it
by the launch latency, which only matters for a session approved in the last few
seconds of a day, and "the day the child started playing" is the honest answer
there anyway.

### Tokens: the part the #155 note said had to be decided

Token balances are **not** a ledger, and that is what drives the answer. Each
gate is a single row (`token_balances`) with one `updated_day` stamp; a gate
without `carry_over` resets *lazily* when that stamp goes stale rather than by a
sweep job. So once midnight has passed there is no "yesterday's balance" left to
settle against — it is already gone by construction.

Passing `billed_day` straight through to the store would have been actively
harmful. `adjust_token_balance` reads the row, resets it if the stamp doesn't
match, and writes the requested day back. Handing it a *past* date makes it
clobber a fresher balance: a caregiver who grants ten minutes at 00:05, while a
session started at 23:50 is still running, would have that grant silently zeroed
when the session settled.

So `settle_tokens` now takes both dates, and they only differ for a session that
crossed midnight:

- `billed_day` answers the **ledger** questions — which day's force-enable
  override granted this session, and whether the balance it drew on still
  exists.
- `today` is what the **store** is told, always. The stamp can therefore never
  move backwards.

And a gate that doesn't carry over is skipped outright when the two disagree.
The balance that session earned and spent from reset at midnight; there is
nothing left to bill, and billing today's balance instead would be precisely the
defect #170 is about, one dimension over. Carry-over gates hold one continuous
balance and settle exactly as before.

The force-enable lookup moving to `billed_day` is the smaller half of the same
point, and it matters on its own: an override is keyed by date, so the grant
that approved a 23:50 session had expired by the time the session ended. Asking
`today` meant `granted` came back false and the child was billed for time the
caregiver had given them.

Cooldowns needed nothing. They are stored as `now + delta` timestamps, not keyed
by date.

## Tests

Five engine tests, in `engine.rs`, with two small helpers (`on_day`, which is
`at` with the day spelled out, and `run_session_between`, which advances the two
clocks by the gap between two wall-clock times):

| test | what it pins |
| --- | --- |
| `usage_is_billed_to_the_day_the_session_started` | the whole session on the start day, nothing on the end day |
| `a_session_across_midnight_leaves_the_new_days_quota_untouched` | what the child notices: `max_run_if_started_now` is the full quota after midnight, and short by the session before it |
| `a_failed_launch_across_midnight_is_billed_to_neither_day` | the #135 exemption still holds on both days |
| `a_carry_over_gate_is_still_spent_across_midnight` | carry-over gates are *not* skipped |
| `a_grant_after_midnight_survives_a_session_that_started_yesterday` | the clobber described above |
| `a_force_enable_from_the_start_day_still_exempts_the_spend` | the override lookup follows the billed day |

Four of them were checked against the pre-fix behaviour (reverting `billed_day`
to `now.date_naive()`, the override lookup to `today`, and disabling the skip)
and all four fail there. The other two are regression guards and pass either
way, which is what they are for.

`cargo test --workspace --all-targets` is green (62 suites) and
`cargo clippy --workspace --all-targets -- -D warnings` is clean.

## Verified end to end, headless

Mock time advances naturally from its starting offset, so crossing midnight for
real only takes a fixture and a minute of waiting.

Fixture (`dev-runtime/midnight-fixture.toml`, since deleted): one always-available
`process` entry `midnight-game` running `/usr/bin/sleep 600`, with
`daily_quota_seconds = 1800`.

```sh
./scripts/shepherd dev headless --config dev-runtime/midnight-fixture.toml \
    --time "2026-04-27 23:59:20"
printf '{"request_id":1,"api_version":1,"method":"launch","params":{"id":"midnight-game"}}\n' \
    | nc -q1 -U dev-runtime/shepherd.sock
# ...wait past mock midnight...
printf '{"request_id":3,"api_version":1,"method":"stop_current","params":{"mode":"graceful"}}\n' \
    | nc -q1 -U dev-runtime/shepherd.sock
```

The daemon log:

```
Session started  session_id=32c91039… entry_id=midnight-game deadline=2026-04-28 00:29:24
Session ended    session_id=32c91039… entry_id=midnight-game duration_secs=129 reason=UserStop
```

Launched at mock 23:59:24 on the 27th, ended at mock 00:01:33 on the 28th. The
`usage` table:

```
midnight-game | 2026-04-27 | 129
```

One row, on the start day. And `list_entries` right afterwards — i.e. on the
28th — reports `max_run_if_started_now = 1800s`: the new day's quota is whole.

Use `nc -q1`; a bare `nc -U` holds the connection open after the response and
looks like a hang.

## Not changed

- **Mid-session evaluation still asks about `now`.** A session that has crossed
  midnight is checked against the new day's quota by `evaluate_entry`, but only
  for the launcher grid, and nothing may launch while a session is current.
  Re-checking the *running* session against the new day is the crossing-midnight
  problem the issue put out of scope.
- **No wire, config, or UI change.** The admin dashboard's usage reporting reads
  the `usage` table by date and simply sees the corrected rows.
