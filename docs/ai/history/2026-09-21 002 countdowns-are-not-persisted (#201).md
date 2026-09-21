# Countdowns are not persisted on hard-power-off events (issue #201)

> Issue: <https://github.com/aarmea/lunchbox/issues/201>

## Prompt

> investigate #201

## Issue text

> Because time is reconciled on shepherdd-driven exits, if an activity is
> "exited" by holding down the power button, the used time won't be recorded by
> shepherdd. This provides a path to bypass the time restrictions (that may also
> end up in disk corruption...)

## Headline finding

**The report is correct, and it understates the problem in two directions.**

1. It is not only a hard power-off. Usage is written to the database **exactly
   once per session, at session end**, and `end_current_session` is only ever
   reached through the event loop. So *any* way lunchboxd stops while a session
   is live loses the whole session — including **`SIGTERM`, `SIGHUP`, and the
   ordinary logout path a device uses to reboot**. Measured: a clean shutdown
   that logs `Shutting down lunchboxd` / `Stopping active session` writes no
   usage row and no `session_ended` audit row.
2. It is not only the countdown. The same single call site also settles **token
   balances and cooldowns** (`settle_session_end`). A lost session returns the
   quota, refunds the tokens, and skips the cooldown.

The database itself is not the weak point. SQLite is running on its defaults —
rollback journal, `synchronous=FULL` — so every commit that *does* happen is
fsynced and survives a power cut. Confirmed against the live dev database:

```
journal_mode: delete
synchronous : 2   (FULL)
```

The gap is entirely that there is nothing to commit until the session ends.

**A crash-recovery facility for exactly this already exists in the tree and is
never called.** `StateSnapshot` / `SessionSnapshot`, the single-row `snapshot`
table, and `save_snapshot`/`load_snapshot` are implemented, tested at the store
level, and plumbed all the way through the custodian's wire protocol — and the
only callers anywhere are tests. `CoreEngine::new` starts at
`current_session: None` unconditionally and nothing reads the snapshot at boot.

> Everything above is the state of the tree *before* this work. What was built
> on top of it is under "What was implemented"; the snapshot facility is what
> the fix is founded on, so it has callers now.

## Where the time lives, and when it is written

Live session state is process memory only, in
`CoreEngine::current_session: Option<ActiveSession>` (`crates/lunchbox-core/src/engine.rs:67`).
Elapsed time is never stored — it is derived on demand from a monotonic anchor:

| field | site | role |
| --- | --- | --- |
| `started_at` | `crates/lunchbox-core/src/session.rs:64` | wall clock; picks the billing day (#170) |
| `started_at_mono` | `crates/lunchbox-core/src/session.rs:67` | enforcement clock |
| `deadline` / `deadline_mono` | `crates/lunchbox-core/src/session.rs:70`, `:73` | display / enforcement |
| `window_ready_at_mono` | `crates/lunchbox-core/src/session.rs:106` | the billing anchor (#135) |

`ActiveSession` does not implement `Serialize`. Nothing writes it anywhere.

There is **one** production call site that persists usage:

- `crates/lunchbox-core/src/engine.rs:1790` — `store.add_usage(…)`, inside
  `end_current_session` (`:1738`), followed by `settle_session_end` (`:1796`)
  for tokens and cooldowns.

`CoreEngine::tick` (`engine.rs:1463`) runs every 100 ms from
`crates/lunchboxd/src/main.rs:2124` and persists **nothing** — it recomputes
availability, emits warnings, and raises `ExpireDue`. At session *start* only an
audit row is written (`engine.rs:1337`); there is no open-session record in any
table anyone reads back.

## Every route into `end_current_session`, and the one that is missing

| path | site | settles? |
| --- | --- | --- |
| activity process exited | `crates/lunchboxd/src/main.rs:2651` → `notify_activity_exited` (`engine.rs:1709`) | yes |
| deliberate stop (HUD "X", admin) | `crates/lunchbox-management/src/service.rs:811` `finish_stop` (`engine.rs:1901`) | yes |
| expiry | `engine.rs:1452` | yes |
| restart/reset in place | suppressed on purpose (`engine.rs:1747`) | n/a |
| **daemon shutdown** | `crates/lunchboxd/src/main.rs:2225-2246` | **no** |

The shutdown block is the defect in code form. It breaks out of the `select!`
loop first (`main.rs:2117`), then takes a **read** lock on the engine and calls
`host.stop(handle, Graceful { 5s })` on the live session. The `HostEvent::Exited`
that this produces is the very thing that would have driven
`end_current_session` — and the loop that would have received it is already
gone. So the activity is stopped correctly and the time it used is dropped.

A hard power-off is the same hole with the stop call also skipped.

## Reproduced, end to end, headless

Fixture: one always-available `process` entry, `daily_quota_seconds = 1800`,
pointed at a wrapper script running `exec tail -f /dev/null` (a bare `sleep`
would be killed by the stop path's `kill_by_command`; see the headless-dev
skill).

### 1. Hard power-off (`SIGKILL`, the closest in-process analogue)

Launched, played 2 min 45 s (`service_state` reported `time_remaining: 1639s` of
1800), then `kill -9` on lunchboxd and its two `sh -c` wrappers.

```
usage    : []
snapshot : []
cooldowns: []
audit    : {"type":"session_started","session_id":"eba3be0a-…","entry_id":"quota-game"}
           (no session_ended)
```

On the next boot, `list_entries` for that entry:

```
enabled               : True
max_run_if_started_now: 1800s
```

The child played, and the device believes the day's quota is untouched. **This
is the bypass the issue describes, reproduced.**

### 2. Clean `SIGTERM` — the same result

Launched, played 57 s, then `kill -TERM`. The daemon shut down properly:

```
INFO lunchboxd: Received SIGTERM, shutting down gracefully
INFO lunchboxd: Shutting down lunchboxd
INFO lunchboxd: Stopping active session session_id=962cc758-…
```

`usage: []`, and no `session_ended` audit row. This matters more than the
power-button case, because it is the *supported* way out: `sway.conf`'s
`Mod4+Shift+Escape` is `pkill -TERM lunchboxd`, which `lunchbox install
sway-config` rewrites to `loginctl terminate-session` — and that ends sway,
which SIGHUPs lunchboxd into the same arm (`main.rs:2085-2097`).

### 3. Sway teardown — the device's own logout path

Launched, played 40 s, tore the compositor down. Same shutdown log, same result.
After the three scenarios the database held **3 `session_started` rows, 0
`session_ended` rows, and an empty `usage` table**.

### 4. Positive control

Same fixture, launch → 35 s → `stop_current`:

```
usage      : [('quota-game', '2026-09-21', 35)]
ended rows : 1
```

The harness bills correctly when the normal path runs, so scenarios 1-3 are
specific to shutdown and crash, not an artefact of the fixture.

## Prompt, continued

> do all 3. for #2, just write the snapshots so that they're present for #3

and, once those were built:

> Make any crash like this one visible as a diagnostic, then commit as
> reviewable chunks and push the PR

## What was implemented

Three pieces, in the order they matter to a child's ledger. The investigation
above proposed delta *usage* writes for the second; the reporter redirected it
to snapshots only, which is both smaller and better — there is still exactly one
`add_usage` per session, so the double-count hazard `RemoteStore` guards against
(`crates/lunchbox-state-proto/src/client.rs:21`) never arises, and the
checkpoint's only reader is the recovery path.

### 1. A clean shutdown settles the session it stops

`crates/lunchboxd/src/main.rs`, the shutdown block. It used to stop the activity
and walk away. Now it runs the same two-phase teardown every other stop does:

```rust
let now_mono = MonotonicInstant::now();   // before the teardown wait
let now = lunchbox_util::now();
if let BeginStopDecision::Stopping { handle, .. } =
    engine.begin_stop(SessionEndReason::ServiceShutdown)
{
    // ...host.stop(handle, Graceful { 5s })...
    engine.finish_stop(now_mono, now);
}
```

The clock is read *before* the up-to-5s teardown, exactly as `stop_current`
does, so the child is not charged for "Closing…".

This is the half with no tamper story at all: a caregiver rebooting the device
was losing the child's time. It needed no new state.

### 2. The running session is checkpointed

`CoreEngine::checkpoint_session`, called from the top of `tick` — the same loop
that decides whether a child's time is up, and for the same reason #172 put the
custodian's heartbeat there. It writes a `StateSnapshot` at most every
`SNAPSHOT_INTERVAL` (30 s), and `last_snapshot_at` is reset when a session
starts so the *first* tick of a session writes one immediately. A power cut ten
seconds in therefore still leaves a record that a session was open, billing
zero; leaving no trace at all would be worse.

`SessionSnapshot` gained two fields:

- `deadline` became `Option`, because unlimited sessions have none.
- **`billable: Duration`** — the crux. It is carried rather than re-derived,
  because the clock that measures it does not survive what the snapshot exists
  for. Billing runs on `CLOCK_MONOTONIC`, which excludes suspend (#155) and the
  pre-window spinner (#135), and a reboot resets it.

Nothing else had to change in the store: the row already existed, the wire
protocol already carried it, and no migration is needed because the table has
been created (and left empty) on every device since it was written.

Failure to write is logged and ignored — a store that cannot be reached must not
take the session down with it.

### 3. Startup settles what the last run did not

`CoreEngine::recover_interrupted_session(now)`, called from lunchboxd's `new`
immediately after the engine is built — before the socket exists, so nothing can
launch ahead of it and nothing can checkpoint over the snapshot it is reading.

It charges the checkpoint's `billable` to `started_at.date_naive()` (#170),
settles tokens and cooldowns through the existing `settle_session_end`, writes
the audit event, and clears the checkpoint.

Three decisions worth recording:

**It charges the checkpoint and no more.** Up to one interval of real play is
not in it. The wall clock could be asked how long ago the checkpoint was, but
that answer is chosen by whoever decided when to switch the device back on, and
it counts time the device spent *off* as play. A bound the child controls is
worse than one that is slightly too small.

**The audit event is stamped at the snapshot's timestamp, not at boot.**
Otherwise a device left off overnight records last night's session as ending at
breakfast. `AuditEvent` is constructed by hand rather than through
`AuditEvent::new`, which stamps `now`.

**`SessionEndReason::Interrupted` is a new variant** rather than a reuse of
`ServiceShutdown`. They are opposites: one is an orderly exit that settled
itself, the other is the record that something took the daemon out from under a
child mid-play. `ServiceShutdown` existed and was unused; it now has its first
caller, in piece 1.

One case is charged where a live session would not be: a launch still failing
when the power went. `end_current_session` exempts `LaunchFailed` (#135) and a
snapshot cannot know that is what it was about to become. Bounded by the launch
itself, and it errs toward charging, which is the right direction here.

### 4. An administrator is told it happened

Recovering the time is not the same as anyone *knowing* it happened, and the
reporter asked for the second thing explicitly. Before this, a power cut reached
one log line on a device nobody can log into; the launcher came up looking
entirely normal and the only trace a parent could see was a quota that was
slightly too generous.

Startup recovery therefore also raises `DiagnosticCode::SessionInterrupted`
(#143's administrator-facing channel — deliberately not the child-facing warning
channel). It names the activity that was open, what was charged, and that up to
`SNAPSHOT_INTERVAL` of it could not be counted. That last part is the reason to
write the message by hand rather than dump the number: a figure presented as
exact would be worse than one presented as a floor, because the gap is precisely
what a child can work with.

Four decisions:

- **`Warning`, not `Critical`.** `Critical` is reserved for "the configuration
  claims a protection the device is not providing". The protection held here and
  the time was recovered; what is degraded is the *accuracy* of the accounting.
- **Subject `Service`, not `Entry`.** The entry is not misconfigured, and the
  precedent (`FirewallNotApplied` vs `FirewallUnenforceable`) is that an
  entry-subject diagnostic renders on the Entries page as a problem *with that
  entry*. The label goes in the message instead.
- **`since` is the snapshot's timestamp**, like the audit event, so a device
  switched on the next morning dates the power cut to the night before.
- **Raised only at startup, and it lives for that boot.** The condition is "the
  previous run ended badly", which is true for exactly as long as it is the most
  recent thing that happened. The registry is in-memory and starts empty, so a
  clean boot clears it without any site having to remember to — which is the
  observed-condition contract in `diagnostics.rs` ("cleared when the same site
  next succeeds") satisfied for free.

No web UI change: `DiagnosticsPage.tsx` renders `message` and `remedy` directly
and has no per-code table to keep in step.

### Not changed

- **No config surface.** `SNAPSHOT_INTERVAL` is a constant with its rationale
  written next to it. Making it configurable means schema, wasm, example config
  and codegen for a number whose only correct value is a judgement about how
  much a power cycle may buy; worth doing if a device ever wants a different
  trade, not before.
- **No *enforcement* consequence for repeated power-cycling.** Visibility is
  now covered from both ends — the audit log records each one as `interrupted`,
  and the diagnostic puts the latest in front of a parent. What is still open is
  whether a pattern should *cost* something (a cooldown, a forfeited quota), and
  that stays open deliberately: it is a supervision-policy judgement for the
  reporter, and it needs a notion of "pattern" that nothing currently keeps.
  Note the diagnostic cannot count across boots — the registry is in-memory —
  so a count would have to come from the audit log, over some chosen window.
- **No resumption.** A recovered session is settled and closed, never restored.

## Tests

Thirteen: nine in `engine.rs` and four in `lunchboxd/src/main.rs`.

Eight of the engine nine use a `power_cut_after` helper that runs a session
through real 1 Hz ticks and then drops the engine with no end at all — no
`end_current_session`, no settlement, nothing but what reached the store — and
hands back a fresh engine on the same store, as the next boot would see it.

| test | what it pins |
| --- | --- |
| `a_session_cut_short_by_a_power_cut_is_still_billed` | the bypass: charged to the last checkpoint, not refunded |
| `a_power_cut_does_not_refund_the_days_quota` | what the child feels — the quota, asserted directly |
| `a_recovered_session_is_not_recovered_twice` | power-cycling again does not re-apply the charge |
| `a_session_that_ended_cleanly_leaves_nothing_to_recover` | a clean end clears the checkpoint, so no double bill |
| `a_power_cut_in_the_first_seconds_still_leaves_a_record` | the immediate first checkpoint |
| `a_recovered_session_is_billed_to_the_day_it_started` | #170 holds through recovery |
| `a_recovered_session_is_written_to_the_audit_log` | `Interrupted`, stamped at last-seen |
| `checkpoints_are_written_on_the_interval_not_every_tick` | it is a bound on loss, not a clock |
| `a_shutdown_settles_the_session_it_stops` | piece 1 at the engine level |

And four on the diagnostic (`interrupted_session_diagnostic_tests`):
`it_names_the_activity_and_what_was_recovered`,
`it_admits_what_could_not_be_counted` (the uncounted bound is in the message,
which is the whole point of writing it by hand), `it_is_a_warning`, and
`it_is_dated_when_the_session_was_last_seen`.

Six of the nine were checked against the pre-fix behaviour by disabling
`checkpoint_session` in `tick`, and all six fail there. The other three
(`..._not_recovered_twice`, `..._ended_cleanly_leaves_nothing`,
`a_shutdown_settles...`) pass either way, which is what regression guards are
for.

`cargo clippy --workspace --all-targets -- -D warnings` is clean and
`cargo fmt --all --check` passes. `cargo test --workspace` is green except for
`lunchbox-http`'s `files` and `files_on_disk` suites (15 failures, mostly
HTTP 507), which **fail identically on a clean checkout** — confirmed by
stashing. Unrelated to this work, and worth its own issue.

## Verified end to end, headless

Same fixture as the reproduction above, on the real stack.

**The checkpoint is live on disk while the session runs** — read straight out of
`snapshot` at 101 s elapsed:

```json
{"timestamp": "2026-09-21T01:07:42-04:00",
 "active_session": {"entry_id": "quota-game", "started_at": "2026-09-21T01:06:12-04:00",
                    "billable": {"secs": 90, "nanos": 252359832}}}
```

**1. Hard power-off.** `kill -9` on the daemon, then reboot:

```
WARN lunchbox_core::engine: Recovered a session the last run never settled;
     charging what the last checkpoint saw entry_id=quota-game billed_secs=90
     last_seen=2026-09-21 01:07:42
```

```
usage    : [('quota-game', '2026-09-21', 90)]
snapshot : None
audit    : 2026-09-21T01:07:42  {"type":"session_ended","reason":{"type":"interrupted"},…}
max_run_if_started_now: 1710s     (was 1800s before the fix)
```

**2. Clean `SIGTERM`.** 45 s session:

```
INFO lunchbox_core::engine: Session ended duration_secs=45 reason=ServiceShutdown
INFO lunchboxd: Settled the active session on shutdown duration_secs=45
usage: [('quota-game', '2026-09-21', 135)]      # 90 + 45
```

**3. No double bill.** Rebooting after that clean shutdown recovers nothing
(`Recovered a session` count: 0) and the ledger stays at 135.

**4. Normal stop still correct.** Launch → stop → billed as before, and the
audit log ends up holding one of each: `['interrupted', 'service_shutdown',
'user_stop']`.

**5. The diagnostic reaches a client.** Read back over the management socket
after a `kill -9` and reboot, alongside the dev stack's pre-existing firewall
one:

```
code    : session_interrupted
subject : {'type': 'service'}
severity: warning
since   : 2026-09-21T07:34:27-04:00        # daemon started at 07:35:10
message : The device stopped without closing "Secret of Monkey Island". 1 min of
          that session was recovered and charged; up to 30s of it could not be
          counted.
remedy  : A power cut or crash does this. If it keeps happening while an activity
          is open, check whether the device is being switched off to win time
          back — the audit log lists these as `interrupted`.
```

`since` predating the daemon's own start is the timestamp rule working. On the
next clean boot the set is back to `['firewall_unenforceable']`, so it clears
itself.


## The "disk corruption" aside

Worth separating from the rest of the issue. The store's own durability is
already correct — defaults give a rollback journal at `synchronous=FULL`, and a
committed `add_usage` is fsynced before it returns. The one thing that is wrong
here is documentation: `crates/lunchbox-store/README.md:33` claims "Automatic
crash recovery via WAL mode", and WAL is not enabled — no `PRAGMA journal_mode`
is set anywhere (`grep PRAGMA` in `sqlite.rs` finds only `table_info`, used by
the migrations). **Corrected rather than implemented**: the default journal is
the safer of the two here, and switching to WAL would *reduce* durability under
a power cut unless `synchronous` were raised back. The README now says what the
code actually does, and says why WAL is not wanted, so the next reader does not
"fix" it.

Anything beyond the database — the filesystem, an activity's own save files —
is outside what Lunchbox controls and is not addressed by any of the above.

## Existing test coverage

Usage accounting is well covered along the paths that work:
`crates/lunchbox-store/src/sqlite.rs:845`, `crates/lunchbox-core/src/engine.rs:5030`
and the #170 family around it, `crates/lunchbox-management/tests/supervision.rs:559`
onwards, `crates/lunchboxd/tests/integration.rs:268`, and
`crates/lunchbox-state-proto/tests/round_trip.rs:102`.

**Nothing covered a lunchboxd crash mid-session, a `SIGTERM` with a live
session, or any persistence of elapsed time before the session ended.**
`sqlite.rs:1168` (`test_snapshot`) was the only exercise of the snapshot path,
and it tested a facility production never called — note that it only ever built
an `active_session: None`, which is why changing `SessionSnapshot` broke no
existing test. The nine tests listed under "Tests" above are what now covers
this.
