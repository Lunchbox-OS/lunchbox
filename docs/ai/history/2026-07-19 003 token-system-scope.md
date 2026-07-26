# Token system — scope and implementation (issue #8)

> Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/8>

## Prompt

> scope out #8

...followed by:

> go ahead and implement the whole thing

The design below was scoped first and then built as described. Differences
between the plan and what landed are called out in "As built" at the end.

## Issue text

> **Token system**
>
> It may be useful to gate whether one activity is available on:
>
> - whether a different activity has been open long enough
> - a manual condition confirmed directly by the caregiver/administrator
> - an automatic condition confirmed by an external API
>
> These may be considered "tokens" that unlock that activity for a certain
> amount of time.

Comment (albert, 2026-07-19):

> Manual conditions can be done using an override, and external conditions can
> be done by an API call, so this tracks just the time-based conditions.
>
> Example: Minecraft and YouTube should not be enabled until at least 30
> minutes of any combination of Cluefinders, typing tutor, Scratch. This is
> likely cleanest to implement when configured against the target activities.

So the issue narrows to **one** mechanism: time earned on a set of *source*
activities unlocks a *target* activity, configured on the target.

## Decisions taken during scoping

| Question | Decision |
|---|---|
| What does earned time grant? | **Earn-and-spend currency.** Source minutes bank a balance on the target; the target's own sessions spend it down. Not a once-per-day threshold. |
| What does the child see while locked? | **Nothing** — the tile stays hidden, matching today's `!enabled` behavior in the grid. No new launcher UI. |
| Scope of v1 | **shepherd-core + config + store only.** Web UI progress and live HUD readouts are follow-ups. |

## Existing architecture this plugs into

Availability has a single chokepoint: `CoreEngine::evaluate_entry()` in
`crates/shepherd-core/src/engine.rs:199`, which both `list_entries()`
(`engine.rs:190`) and `request_launch()` (`engine.rs:380`) go through — so one
new check covers display *and* launch enforcement. Session length is clamped
separately in `compute_max_duration()` (`engine.rs:342`).

Usage is already persisted per `(entry_id, local date)` in the `usage` table
(`crates/shepherd-store/src/sqlite.rs:53`), written **only at session end**, in
exactly two places: `notify_session_exited()` (`engine.rs:598`) and
`stop_current()` (`engine.rs:652`). Those are the two hooks where tokens accrue
and get spent. Daily reset is implicit — the date key rolls over at local
midnight; there is no reset job.

`SessionActive` (`engine.rs:287`) disables every other entry while anything is
running, so there is no concurrency to reason about: at most one activity earns
or spends at a time.

## Proposed design

### Config

New optional block on the **target** entry:

```toml
[[entries]]
id = "minecraft"
label = "Minecraft"

[entries.tokens]
# Sessions on these entries bank time toward this one.
from = ["cluefinders", "typing-tutor", "scratch"]
# Target seconds earned per source second. Default 1.0.
earn_ratio = 1.0
# Balance required before the entry unlocks at all. Default 0 (any balance
# above zero unlocks).
minimum_seconds = 1800
# Ceiling on banked time, so a long Saturday of Scratch doesn't bank a week of
# Minecraft. 0 = unlimited (default).
max_balance_seconds = 3600
# Whether the balance survives local midnight. Default false (resets daily),
# consistent with how daily quota works.
carry_over = false
```

Layers to touch, mirroring how `requires_input` (#96) threads through:

- `crates/shepherd-config/src/schema.rs` — `RawTokens` struct + `tokens:
  Option<RawTokens>` on `RawEntry` (near `:99`).
- `crates/shepherd-config/src/policy.rs` — `TokensPolicy` on `Entry` (near
  `:347`) with `Duration`-typed fields, plus a `convert_tokens()` next to
  `convert_limits()` (`:859`).
- `crates/shepherd-config/src/validation.rs` — this needs a **cross-entry**
  pass in `validate_config()` (`:39`), not `validate_entry()`, since it must
  resolve ids against the whole config.

Validation rules:

1. every id in `from` must exist — error
2. `from` must not contain the entry's own id — error
3. `from` must be non-empty when `[entries.tokens]` is present — error
4. `earn_ratio` must be finite and `> 0` — error
5. `minimum_seconds <= max_balance_seconds` when both are non-zero — error
   (otherwise the entry is permanently unreachable)
6. a `from` entry that is itself token-gated is *allowed* (chains are
   coherent), but worth a warning since it is easy to configure a dead end

### Storage

New table, lazily reset rather than swept:

```sql
CREATE TABLE IF NOT EXISTS token_balances (
    entry_id     TEXT PRIMARY KEY,
    balance_secs INTEGER NOT NULL DEFAULT 0,
    updated_day  TEXT NOT NULL
);
```

`updated_day` is the local date of the last mutation. When `carry_over = false`
and `updated_day != today`, reads return `0` and the next write overwrites the
row — the same trick the `usage` table gets for free from its composite key, so
no cron job and no migration hazard.

`Store` trait (`crates/shepherd-store/src/traits.rs`) gains:

```rust
fn get_token_balance(&self, entry_id: &EntryId, day: NaiveDate) -> StoreResult<Duration>;
fn adjust_token_balance(&self, entry_id: &EntryId, day: NaiveDate, delta_secs: i64)
    -> StoreResult<Duration>;  // saturates at 0, returns the new balance
```

Capping at `max_balance_seconds` stays in the engine, since the store has no
view of policy.

### Engine

**Gate** — new check in `evaluate_entry()` after the daily-quota block
(`engine.rs:305`), guarded by `!manually_enabled` exactly like quota is:

```rust
ReasonCode::TokensInsufficient { balance: Duration, required: Duration }
```

Unlock condition is `balance > 0 && balance >= minimum_seconds`.

**Clamp** — in `compute_max_duration()` (`engine.rs:342`), clamp the session to
the remaining balance the same way daily quota is clamped, again skipped when
`manually_enabled`. This is what makes the currency real: the child can never
spend more than they banked, and the existing warning/expiry machinery gives
them the countdown for free.

**Settle** — factor a `settle_tokens(&self, ended_entry_id, duration, today)`
helper called from both `notify_session_exited()` and `stop_current()`, right
next to the existing `add_usage()` calls (which are already duplicated across
those two functions):

- *earn*: for every entry whose `tokens.from` contains `ended_entry_id`, add
  `floor(duration * earn_ratio)`, capped at `max_balance_seconds`
- *spend*: if the ended entry is itself token-gated, subtract `duration`
  (saturating at 0)

An entry can legitimately be both, in which case both halves run.

**Override interaction** — `availability = Some(true)` bypasses the gate and
the clamp, consistent with how it already bypasses windows and daily quota
(see `docs/ai/history/2026-06-24 001 overrides-bypass-daily-quota.md`). A
force-enabled session should **not** deduct balance: the caregiver granted it,
the child didn't pay for it. Worth confirming, but it is the semantics that
makes "Enable Today" mean the same thing everywhere.

### Downstream

`crates/shepherd-launcher-ui/src/client.rs:236` (`reason_to_message`) is an
exhaustive match — adding the variant breaks the build there until updated,
which is the compiler enforcing the checklist. `tile.rs:164` needs nothing
special; the grid hides the tile anyway (`grid.rs:111`).

`shepherd-webui/src/api/types.ts:41` mirrors `ReasonCode` in TypeScript and is
**already stale** (missing `not_ready` and `required_input_unavailable`). Out of
v1 scope, but worth a drive-by fix while the file is open. Regenerate
`docs/rpc-schema.json` if the RPC surface shifts.

## Suggested PR split

1. **store + config** — table, trait methods, schema/policy/validation, unit
   tests. No behavior change yet.
2. **engine** — reason code, gate, clamp, `settle_tokens`, engine tests.
3. **docs** — `config.example.toml` example (validated), crate READMEs,
   launcher string.

## Tests

- `shepherd-store` (`sqlite.rs:530` is the template): accrual, saturation at 0,
  lazy reset across a day boundary, `carry_over` both ways.
- `shepherd-config`: each validation rule above, plus round-trip of the example
  block.
- `shepherd-core` (`test_enable_override_bypasses_daily_quota`, `engine.rs:1559`,
  is the closest template — it pre-seeds state via the store): locked at zero
  balance; a source session banks time; unlock at `minimum_seconds`; multiple
  sources accumulate; `max_run_if_started_now` clamped to balance; balance spent
  on session end; `max_balance_seconds` cap; override bypasses without
  deducting.
- e2e: there is no existing quota/window e2e test, so this would be new ground.
  Recommend skipping in v1 rather than building the first one here.

## Known gaps / follow-ups

- **No feedback to the child.** A locked target is simply absent, and it
  vanishes again the moment the balance runs out. The feature only teaches
  "play Scratch to get Minecraft" if a caregiver says so out loud. A locked tile
  with a progress hint (`18 / 30 min`) is the obvious follow-up and needs
  `grid.rs:111` to stop hiding disabled tiles.
- **No caregiver visibility.** Nothing shows a banked balance in the web UI. A
  `token_delta_seconds` field on `DailyOverride`, alongside the existing
  `quota_delta_seconds`, is the natural way to let a caregiver grant or revoke
  tokens.
- **No live accrual.** Because usage lands only at session end, a running
  Scratch session banks nothing until it stops. Fine for the gate; it is the
  blocker for any HUD readout, which would need
  `session.duration_so_far()` folded into the balance at read time.
- **Crash loses the session.** If shepherdd dies mid-session neither usage nor
  tokens are recorded. Pre-existing behavior, not made worse here.
- The issue's other two bullets (manual and external-API conditions) are
  explicitly out of scope per the comment — overrides and the management API
  already cover them.

## As built

Landed as designed, with these deltas:

- **Validation lives in `validate_entry`, not a new pass.** `validate_entry`
  already receives the whole `RawConfig` (it needs it for `default_max_run`), so
  the cross-entry ID check fits there as `validate_tokens(tokens, entry, config)`
  rather than needing a separate whole-config pass in `validate_config`.
- **The "chained gate" warning was dropped.** `validate_config` returns only
  `Vec<ValidationError>` with no warning severity, and a chain (A gated on B,
  B gated on C) is legal and coherent, so there was nothing to report at error
  level. Documented instead.
- **Override exemption reads the override at settle time.** Rather than adding a
  `tokens_exempt` field to `SessionPlan` — which crosses the IPC boundary — the
  spend half of `settle_tokens` checks
  `get_daily_override(entry_id, today).availability == Some(true)`. The override
  is day-scoped and persisted, so this is a faithful proxy. The one behavioral
  wrinkle: an override set *mid-session* also exempts that session, which is
  arguably the right answer anyway.
- **Earning is never exempt.** Only spending is. A source session run under an
  override still banks time: the child did the work, and the override only
  governed whether the source activity was allowed to run at all.
- **The `max_balance` ceiling is applied in the engine**, not the store, since
  the store has no view of policy. `settle_tokens` adds the earned time and then
  claws back any excess.
- **The web UI's TS `ReasonCode` mirror was fixed while open.** It was missing
  `not_ready` and `required_input_unavailable` before this change; all three
  variants (including `tokens_insufficient`) are now present in
  `shepherd-webui/src/api/types.ts` with labels. This is only the type mirror —
  the balance-progress UI is still a follow-up.

### Interaction with the other time restrictions

Documented in the "How the limits interact" section of
`crates/shepherd-config/README.md`, with the config-authoring cautions repeated
inline in `config.example.toml`. The short version: visibility is a plain AND
across every check, session length is the minimum of every applicable cap, and
the trap worth knowing is that a `daily_quota_seconds` (or a closed availability
window) on a gated entry can strand earned tokens — hidden despite a healthy
balance, and expiring at midnight unless `carry_over = true`.

### Verification

`cargo test` across shepherd-config / -store / -core / -management / -http /
shepherdd, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --all`, and
`tsc --noEmit` for the web UI all pass. `config.example.toml` passes
`shepherd config validate`.

End-to-end via the headless dev session (`headless-dev` skill) against a
two-entry fixture (`practice`, and `reward` gated on it at `earn_ratio = 60`):
the launcher showed only `practice`; after a 127-second `practice` session ended,
`reward` appeared in the grid, and `list_entries` reported
`max_run_if_started_now = 7620s` — 127 × 60, exactly the banked balance — on an
entry with no configured `max_run`. That confirms the whole chain: TOML → policy
→ engine gate → session cap → launcher grid.
