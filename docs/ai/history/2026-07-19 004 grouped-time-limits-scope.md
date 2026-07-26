# Grouped time limits — scope and implementation (issue #5)

> Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/5>
> Builds on: [`2026-07-19 003 token-system-scope.md`](2026-07-19%20003%20token-system-scope.md) (issue #8)

## Prompt

> commit this, then prepare to also implement #5 on top

...followed by:

> just do the single subject-keyed table and nuke the local table here -- the
> code hasn't left this dev box yet. the rest looks reasonable, just also do the
> group-level overrides

The design below was scoped first and then built. "As built" at the end records
where the implementation diverged.

## Issue text

> **Grouped time limits**
>
> It may be useful to group activities together by category so that they can
> share the same availability schedule and time limit.
>
> For example, there could be a group of games that are less educational in
> nature or demand more attention. The administrator may want to set one
> configuration for these that allows them in shorter bursts and then removes
> them altogether once the usage *total* of all of them exceeds the set amount.

## Decisions taken during scoping

| Question | Decision |
|---|---|
| Membership | **One group per activity** (`group = "id"`). One extra window check, one extra quota check; no looping. |
| What a group carries | **Availability window, daily quota, per-session `max_run`, and cooldown** — the full limit set, shared. |
| Token interop | A group can be a token **source** (`from = ["group:educational"]`) **and** a token **destination** (`[groups.tokens]` gates every member). |
| Composition | **Strictest wins.** A member must satisfy both its own limits and its group's; the session is capped by the tighter of each. Consistent with how every other limit already composes. |

## Config shape

A new top-level `[[groups]]` array, and one new field on entries:

```toml
[[groups]]
id = "attention-heavy"
label = "Games"

# Same schedule for every member, instead of repeating it per entry.
[groups.availability]
[[groups.availability.windows]]
days = "weekends"
start = "10:00"
end = "18:00"

[groups.limits]
max_run_seconds = 900        # short bursts, per session, for any member
daily_quota_seconds = 3600   # COMBINED across all members
cooldown_seconds = 600       # any member's session cools down the whole group

# A group can itself be token-gated: earning unlocks the whole category.
[groups.tokens]
from = ["group:educational"]
earn_ratio = 0.5
minimum_seconds = 600

[[entries]]
id = "steam-celeste"
group = "attention-heavy"
```

`config_version` stays at 1 — this is purely additive.

## Layers to touch

Same spine as #8, one layer deeper.

**`shepherd-config`**
- `schema.rs`: `RawGroup { id, label, availability, limits, tokens }`, `groups:
  Vec<RawGroup>` on `RawConfig`, `group: Option<String>` on `RawEntry`.
- `policy.rs`: `Group { id: GroupId, label, availability, limits, tokens }`,
  `groups: Vec<Group>` on `Policy` plus a `get_group()` lookup mirroring
  `get_entry()`, and `group: Option<GroupId>` on `Entry`.
- `validation.rs`: unique group IDs; `entry.group` resolves; group limits follow
  the same rules as entry limits; token rules from #8 extended to group subjects
  (see below).
- `GroupId` newtype in `shepherd-util/src/ids.rs` alongside `EntryId`.

**`shepherd-store`** — group cooldowns and group token balances need somewhere to
live. See "Storage" below; this is the one genuinely awkward part.

**`shepherd-core`** — `evaluate_entry` gains the group checks,
`compute_max_duration` the group clamps, and session end must settle the group's
cooldown and token balance alongside the entry's.

**`shepherd-api`** — new reason code(s), see below.

**Downstream** — `reason_to_message` in `shepherd-launcher-ui/src/client.rs` is
an exhaustive match and will fail to compile until updated; the web UI's TS
mirror in `shepherd-webui/src/api/types.ts` needs the same variants.

## Storage

`cooldowns` and `token_balances` are both keyed by a bare entry ID. Groups need
the same two pieces of state.

**The constraint that decides this:** `SqliteStore::init_schema` is a batch of
`CREATE TABLE IF NOT EXISTS` with no migration mechanism, no `user_version`, and
no `ALTER TABLE` anywhere. Re-keying an existing table is therefore a silent
no-op on any database that already exists — the old table persists with its old
shape and the new DDL does nothing. That rules out changing `cooldowns` in
place, and it rules out reshaping `token_balances` too: it may be one commit old
and unreleased, but any dev database that has already run the new code (this
machine included, from the headless verification) has the entry-keyed version.

**Recommendation: parallel group-keyed tables.**

```sql
CREATE TABLE IF NOT EXISTS group_cooldowns (
    group_id TEXT PRIMARY KEY,
    until TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS group_token_balances (
    group_id TEXT PRIMARY KEY,
    balance_secs INTEGER NOT NULL DEFAULT 0,
    updated_day TEXT NOT NULL
);
```

The `Store` trait gains group-keyed twins of the four existing methods. To keep
that from being copy-paste, the SQLite implementations of both the entry and
group variants should delegate to private helpers parameterised by table and key
column — the bodies are identical apart from those two strings.

The alternative — a single subject-keyed table (`"entry:x"` / `"group:y"`) — is
the cleaner data model and worth doing if a real migration mechanism ever lands.
It is not worth hand-rolling one for this.

Group **quota** needs no new storage: it is the sum of member usage for the day,
which `Store::get_all_usage_for_date` already returns in one query. That method
has been present and unused since before #8; this is what it was for.

## Evaluation

In `evaluate_entry`, after resolving `entry.group` to a `&Group`, four checks
mirror their entry-level twins and are skipped under a force-enable override
exactly where the entry-level ones are:

| Check | Skipped by `availability = true`? |
|---|---|
| Group availability window | yes (matches entry window) |
| Group daily quota (summed over members) | yes (matches entry quota) |
| Group cooldown | **no** (matches entry cooldown) |
| Group token gate | yes (matches entry token gate) |

`compute_max_duration` gains four more clamps: group `max_run`, group window
remaining, group quota remaining, and group token balance. With "strictest
wins", the result is just `min` over a longer list.

### Reason codes

Rather than four near-duplicate variants, wrap the existing ones:

```rust
/// A restriction that comes from the entry's group rather than the entry.
GroupRestricted { group: GroupId, reason: Box<ReasonCode> },
```

The UI unwraps once and renders "Games: daily limit reached". Every future
entry-level reason then works at group level for free. The alternative — four
flat `GroupQuotaExhausted { … }`-style variants — is easier to pattern-match but
has to be extended by hand each time.

## Token interop (on top of #8)

`tokens.from` entries gain a `group:` prefix, so a source is either an entry ID
or a group ID. `TokensPolicy.from` becomes `Vec<TokenSource>` where
`TokenSource = Entry(EntryId) | Group(GroupId)`.

`settle_tokens` extends on both halves:

- **Earn** — a target banks time if its `from` names the ended entry *or* the
  ended entry's group.
- **Spend** — if the ended entry's group is token-gated, the group's balance is
  spent, under the same force-enable exemption as #8.

Validation rules to add:

1. a `group:` source must name an existing group
2. a group's own token gate cannot list itself as a source
3. an entry cannot list its own group as a source — it would let the activity
   unlock itself, the group-level equivalent of the self-reference rule from #8

**Worth flagging:** an entry that is token-gated *and* sits in a token-gated
group spends both balances for the same session. That is coherent — two separate
budgets, both paid — but it is easy to configure by accident and hard to explain
to a child. The docs should steer toward gating at one level or the other.

## Interaction notes for the docs

The "How the limits interact" section added to `crates/shepherd-config/README.md`
for #8 needs a group column, plus these:

- **Group quota is consumed by whichever member is played**, so one game can burn
  the whole category's budget. That is the point of the issue, but say it out
  loud.
- **A group cooldown blocks hopping between members** to dodge a per-entry
  cooldown, which is the main reason to set one.
- **Entries with no `group` are unaffected** by all of this.
- The #8 caution about a daily quota stranding earned tokens applies at group
  level too, and more sharply: a group quota can strand tokens earned toward the
  whole category.

## Suggested PR split

1. **config** — `[[groups]]`, `entry.group`, `GroupId`, validation. No behavior.
2. **store** — group cooldown and group token-balance tables and methods.
3. **engine** — group window / quota / `max_run` / cooldown gates and clamps,
   `GroupRestricted`, launcher + web UI reason plumbing.
4. **token interop** — `group:` sources, group as token destination.
5. **docs** — `config.example.toml` (must pass validation), crate READMEs, the
   interaction section.

## Tests

Mirrors #8's coverage, one level up:

- **store**: group cooldown round-trip; group balance accrual, saturation, lazy
  daily reset, carry-over.
- **config**: unknown group reference, duplicate group ID, `group:` source
  resolution, group self-reference, entry sourcing its own group.
- **engine**: combined quota hides *every* member once spent; one member's usage
  counts toward another's cap; group `max_run` caps a member's session; group
  cooldown blocks a *different* member; group window composes with a member's own
  window (strictest wins); group token gate unlocks all members; force-enable on
  one member bypasses the group quota without spending group tokens.
- **e2e**: still no existing quota/window e2e to extend; verify with the headless
  dev session against a fixture instead, as #8 did.

## Open follow-ups (not v1)

- **Group-level overrides** — "enable this whole category today", the group
  analogue of `DailyOverride`. Currently a caregiver has to override each member.
  Probably the first thing wanted in practice.
- **Group-level `quota_delta_seconds`** — same reasoning.
- **Surfacing group state** — the launcher hides unavailable entries, so a spent
  category simply vanishes with no explanation. Same gap as #8, same fix
  (progress on a locked tile), and the two should be designed together.

## As built

### Storage: subject-keyed after all, with no data migration

The scope recommended parallel `group_*` tables because re-keying a shipped table
is a silent no-op under `CREATE TABLE IF NOT EXISTS`. The single subject-keyed
table turned out to be reachable without that hazard, because of one choice in
the key format:

**`LimitSubject::Entry` renders as the bare entry ID; only groups take the
`group:` prefix.**

That makes every pre-existing row *already valid* under the new scheme. The
migration is therefore metadata-only — `rename_legacy_key_column` renames
`entry_id` to `subject` on `cooldowns` and `daily_overrides` via `ALTER TABLE …
RENAME COLUMN`, guarded by a `PRAGMA table_info` check so it is a no-op on fresh
or already-migrated databases. No row is read or rewritten, and no data can be
lost. It runs *before* the `CREATE`s, since those are no-ops against an existing
legacy table.

The same property means existing API callers keep working: `upsert_override`
with a bare `"tuxmath"` still resolves to that entry. `FromStr` is infallible for
this reason. Entry and group IDs are both forbidden from starting with `group:`
at validation time, which is what keeps the encoding unambiguous.

`token_balances` was reshaped freely (it is one commit old and unreleased); the
local dev database was recreated rather than migrated, per the instruction.

### Group-level overrides

Folded into v1 rather than left as a follow-up. `daily_overrides` is keyed by
subject, and the four override RPCs take a `LimitSubject`, so
`upsert_override {"id": "group:games", "availability": false}` switches off a
whole category. Semantics:

- a force-**disable** on the group short-circuits every member, reported as
  `GroupRestricted(ManuallyDisabled)`
- a force-**enable** on the group lifts the group's limits *and* each member's
  own — enabling a category for the day means its activities are on today,
  whatever their individual schedules say
- `quota_delta_seconds` on a group adjusts the *combined* quota

This is a **breaking wire change**: `DailyOverride.entry_id` is now
`DailyOverride.subject`. The web UI and the dispatch tests were updated; the
generated RPC schema was regenerated.

### Other deltas from the scope

- **`GroupRestricted` carries the group's `label`** as well as its ID, so the UI
  can say "Games: daily limit reached" without a policy lookup.
- **Validation lives in `validate_group` + a shared `validate_tokens`**
  parameterised by a `TokenGateOwner` enum, rather than duplicated per owner.
- **A group has no service-level `max_run` default.** An absent `[groups.limits]`
  means "no group-level cap", not "one hour" — otherwise every group would
  silently impose the service default on its members.
- **Two self-unlock rules were added** beyond the scope's list: an entry cannot
  be gated on the group it belongs to, and a group cannot be gated on one of its
  own members. Both are the group-level form of #8's self-reference rule.

### Verification

`cargo test` across the workspace (excluding the e2e and Android crates), `cargo
clippy --all-targets -- -D warnings`, `cargo fmt --all`, and `tsc --noEmit` all
pass. `config.example.toml` passes `shepherd config validate`.

New coverage: 7 engine tests (shared quota across members, group `max_run`,
cooldown blocking a *sibling*, group token gate unlocking all members, a group as
a token source, group override enable/disable, group quota delta), 7 config
validation tests, 2 `LimitSubject` round-trip tests, 1 dispatch test for the
group-override wire contract, and — most importantly —
`test_legacy_entry_keyed_tables_are_migrated_in_place`, which builds a real
pre-groups database on disk, opens it, and asserts the old cooldown and override
are still readable and that group-keyed rows now coexist with them.

End-to-end via the headless dev session against a three-entry fixture: both
members of a category reported `max_run` clamped to the group's combined 120s
quota while the ungrouped entry stayed uncapped; after one member's session the
whole category disappeared from the launcher on the shared cooldown, and the
ungrouped entry remained.

## Management-app support (follow-up in the same branch)

The initial group implementation left both management apps blind to groups —
`EntryView` carried no group, there was no way to enumerate categories, and
every override control addressed an entry. Auditing that also turned up a
pre-existing break in the Android companion.

### Android repair (commit `4360c02`)

The companion's `ReasonCode` mirror had drifted **four** variants behind the
device: `not_ready` (#76) and `required_input_unavailable` (#96) were already
missing before this branch added `tokens_insufficient` and `group_restricted`.
kotlinx.serialization throws on an unknown polymorphic discriminator —
`ignoreUnknownKeys` only covers unknown *fields* — and `reasons` is nested
inside `EntryView`, so a single unrecognised reason failed the decode of the
whole `list_entries` response.

The fix registers `ReasonCode.Unknown` as the polymorphic default, which
addresses the class of bug rather than the instance. Note the API is
`polymorphic(ReasonCode::class) { defaultDeserializer { … } }`; the flatter
`polymorphicDefaultDeserializer` does not resolve on kotlinx 1.7.3.

`DailyOverride` also still read `entry_id`. Worse than the decode failure was
how it surfaced: `loadOverride` swallowed the exception with
`runCatching{}.getOrNull()`, making a failed load indistinguishable from "no
override set" — the editor rendered a blank form over a real override, and
saving would have silently overwritten it. It now returns a `Result` and the
UI disables Save/Clear when the load failed.

**Lesson for the codegen:** `RpcMethods.kt` is method-name constants only, and
`ManagementClient` uses raw string literals rather than those constants, so the
codegen drift test gives *zero* protection against payload-shape drift. The
wire tests in `WireTest.kt` are the only guard, which is why every new reason
variant and the subject-keyed override now have one.

### Group UI (commits `938e091`, `<this>`)

`EntryView` gained `group`, and a new `GroupView` / `list_groups` RPC reports
what no single member can answer: members, combined usage against the combined
quota, the cap the group's limits impose, and its current restrictions with the
reasons *unwrapped* (`GroupRestricted` is only meaningful wrapping a member's
view).

Both apps now show a Categories section above Activities and let a caregiver
override a whole category. The web UI generalises its four override mutations
to a `LimitTarget` (subject + label) so category cards drive the same code path
as activity cards; Android extracts `OverrideSection` into
`ui/override/OverrideSection.kt` keyed by subject, reused by the entry and
group detail screens. Activity rows in both apps name their category.

### Verification gap

The web UI was verified visually against the real stack (Chromium headless —
Firefox's `--screenshot` fires on `load`, before React Query resolves, and
Chromium needs `--timeout` rather than `--virtual-time-budget` because the page
holds an open SSE stream that never goes network-idle).

**The Android UI was not visually verified.** It needs an emulator plus a real
BLE device; CI runs JVM unit tests only and the pair/claim path is a documented
manual smoke test. Coverage there is the wire tests plus compilation — the
rendering is unexercised.
