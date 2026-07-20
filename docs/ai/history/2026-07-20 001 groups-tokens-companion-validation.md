# Groups + tokens: on-device validation against the Android companion

> Branch: `u/albert/token-system` (issues [#5], [#8])
> Follows: [`2026-07-19 003 token-system-scope.md`](2026-07-19%20003%20token-system-scope.md),
> [`2026-07-19 004 grouped-time-limits-scope.md`](2026-07-19%20004%20grouped-time-limits-scope.md)

## Prompt

> Validate the groups and tokens implementation on this branch, particularly wrt
> the Android companion app. The connected phone should be already authorized via
> adb and paired over BLE, with a working shepherd bond under the shepherd-kiosk
> user.

This closes the "Verification gap" noted at the end of the grouped-time-limits
doc: the Android UI had never been exercised against a real device.

## How it was driven

The companion talks BLE to the daemon, and its claim record lives in
`~shepherd-kiosk/.local/share/shepherdd/admin.toml`, so the session had to run as
**shepherd-kiosk** for the existing bond to be usable:

```sh
setfacl -m u:shepherd-kiosk:x /home/shepherd-admin        # repo traversal for the target user
./scripts/shepherd dev headless --no-build \
    --user shepherd-kiosk --config dev-runtime/groups-tokens-fixture.toml
```

Two channels were used together:

- **The phone** — `adb install -r app/build/outputs/apk/debug/app-debug.apk`
  hot-swaps over the bond (same debug signature), then `adb shell input tap/swipe`
  plus `adb exec-out screencap -p` to drive and read the UI.
- **The daemon's HTTP management API** as the oracle — same `ManagementService`
  behind the same dispatch, so `list_entries` / `list_groups` / `upsert_override`
  give an exact expected value to compare each screen against. The fixture binds
  it to `127.0.0.1:8080`; the bearer token is `http_token` from `admin.toml`.

The fixture (`dev-runtime/groups-tokens-fixture.toml`, gitignored) is six `foot`
entries: a `learning` category (quota 600 s, max_run 300 s), a `games` category
token-gated on `from = ["group:learning"]` (`minimum_seconds = 60`, quota 300 s,
max_run 120 s, cooldown 300 s), an entry-level gate (`reward-solo`,
`earn_ratio = 2.0`, `minimum_seconds = 30`), and an ungrouped control.

## What works (verified end to end)

- **The legacy DB migration works on a real production database.** The kiosk's
  `shepherdd.db` was a genuine pre-groups DB (`cooldowns.entry_id`,
  `daily_overrides.entry_id`). First boot of this branch renamed both columns to
  `subject` in place and the historical `terminal` override survived intact.
- **Categories UI.** The companion shows a Categories section above Activities,
  each row with combined usage against the combined quota, member count, status
  badge, and the group's own reason; activity rows name their category.
- **Group detail screen.** Shared-limits card, member list, and a subject-keyed
  override editor.
- **Group overrides from the phone.** `Allow` + Save wrote
  `daily_overrides('group:games', …)` and immediately unblocked both members
  (each capped at the group's 120 s `max_run`); `Clear` deleted the row and the
  gate reappeared. Save/Clear enablement and the "Override saved." snackbar
  behaved as written.
- **Earning and spending.** A 76 s `practice-typing` session banked
  `group:games = 76 s` and `reward-solo = 152 s` (ratio 2.0), and the members'
  `max_run_if_started_now` became `min(group max_run 120, balance 76) = 76`.
- **Live refresh.** The `SessionEnded` → `refreshGroups()` path works: the phone
  moved Games from "Blocked / Needs 1m earned" to "Available" without user
  action, and picked up a `reload_config` that added a new category.
- **Group cooldown and group force-disable.** One member's session armed
  `cooldowns('group:games')` and blocked its *sibling*, rendered as
  "Games: Cooling down — available 12:12 AM"; a group force-disable short-circuited
  every member as `GroupRestricted(ManuallyDisabled)`; a group `quota_delta_seconds`
  moved the *combined* quota (600 → 720 s).
- **Reason decoding.** `tokens_insufficient` and `group_restricted` render
  correctly on the phone ("Games: Needs 1m earned (0s banked)"), i.e. the
  `ReasonCode.Unknown` polymorphic-default repair in `4360c02` holds.

## Defects found

### 1. A force-enable at the *other* level still spends tokens (functional)

Evaluation treats an override on either the entry **or** its group as a
force-enable (`engine.rs:307`), lifting both the gate and the clamp. Settlement
only exempts the *same* subject (`engine.rs:707`), so:

| Setup | Observed |
|---|---|
| `upsert_override {"id": "game-a", "availability": true}`, member of token-gated group `games` | group balance 54 s → **38 s** after a 16 s session |
| `upsert_override {"id": "group:rewards", "availability": true}`, member `reward-solo` has its own gate | entry balance 152 s → **136 s**, and the session was approved for a **full hour** (clamp lifted) with only 152 s banked |

Both contradict the scope docs ("a force-enabled session should not deduct
balance"; "force-enable on one member bypasses the group quota without spending
group tokens"). The second is the worse of the two: the gate *and* the clamp are
lifted, so a long granted session drains the balance to zero.

Fix: compute the effective grant flag once — the same
`entry_override == Some(true) || group_override == Some(true)` expression used at
`engine.rs:307` — and pass it into `settle_tokens` for both spend halves.

### 2. A partial spend re-locks the gate and strands the balance (semantic)

The clamp lets a session spend the *whole* balance, but the gate re-checks
`balance >= minimum_seconds` on every evaluation (`policy.rs:578`). Observed: with
`minimum_seconds = 60` and 76 s banked, a 22 s session left 54 s — and the
category vanished again with 54 s stranded, which `carry_over = false` destroys at
midnight. The child earned it and cannot spend it.

Either clamp to `balance - minimum` (spend only above the threshold) or make the
gate ratchet (unlock at `minimum`, stay unlocked while `balance > 0`). Worth
deciding explicitly and documenting in `crates/shepherd-config/README.md`.

### 3. `token_balances` is not covered by the key-column migration (silent)

`sqlite.rs:73-74` migrates `cooldowns` and `daily_overrides` but not
`token_balances`, whose #8-era shape was `entry_id TEXT PRIMARY KEY`. Reproduced
against a hand-built legacy DB: the daemon boots with **no error line**, and an
entry with 5000 s banked reports `tokens_insufficient(balance = 0)` — every
token-gated entry and group is permanently locked. The reads are swallowed
(`unwrap_or(Duration::ZERO)` in `engine.rs:645`, `let _ =` on the writes), so
there is nothing to diagnose from.

Only databases created by the intermediate #8 commit are affected (the local dev
DB was recreated by hand, and the kiosk DB predates #8), but
`rename_legacy_key_column(&conn, "token_balances")` is one line and removes the
hazard. Logging those store errors at `warn!` would also make the class of
failure visible.

### 4. `GroupView.max_run_if_started_now` ignores a force-enable (reporting)

`list_groups` clamps by window/quota/token balance unconditionally
(`engine.rs:240`), so a token-locked or force-enabled category reports
`Some(0)`. The companion renders that literally as **"Up to 0s per session"** on
the group detail screen while the members are actually capped at 120 s. Screenshot
evidence in this session. Either honour the override there as `evaluate_entry`
does, or have the UI suppress the line when the group is disabled.

## Fixes applied

All four were fixed on this branch after the validation pass, at the user's
request ("fix all four, ratchet the gate").

1. **Cross-level override exemption.** `settle_tokens` computes one `granted`
   flag — a force-enable on the ended entry *or* on its group — and uses it for
   both spend halves, so it now matches the `manually_enabled` expression that
   lifts the gate and the clamp in `evaluate_entry`.
2. **The gate ratchets.** `TokensPolicy::unlocked(balance, ratcheted)` opens at
   `minimum_seconds` and stays open while the balance lasts. The flag is a
   `ratcheted` column on `token_balances`: the engine sets it in `earn_tokens`
   (the only place a balance grows, and the only place that knows the
   threshold), the store clears it when a balance is spent to zero, and it
   expires with the balance at midnight unless `carry_over`.
3. **`token_balances` is migrated**, and the store errors that hid the problem
   are now logged at `warn!` rather than swallowed. A new `add_missing_column`
   helper applies the `ratcheted` column to databases that already have the
   table, on the same `PRAGMA table_info` guard.
4. **`group_max_duration` takes `manually_enabled`**, so a force-enabled category
   reports the per-session cap its members actually get.

Regression coverage: 4 engine tests (both cross-level exemption directions, the
ratchet including its release at zero, and the group cap under an override) and
3 store tests (ratchet persistence, its midnight reset, and a legacy
`token_balances` table folded into the existing migration test). Full workspace
`cargo test`, `cargo clippy --all-targets -- -D warnings` and `cargo fmt` pass,
and `config.example.toml` still validates.

Re-verified on the device against the same fixture: a 22 s spend from a 76 s
balance left the category unlocked at 54 s with `ratcheted = 1` persisted;
neither cross-level override spent a balance (54 s and 152 s both unchanged);
and the group screen now reads **"Up to 2m per session"** where it previously
said "Up to 0s".

## Manual token grants (follow-up in the same branch)

> Prompt: *"also add an API+UI to manually add tokens"*

The #8 scope listed "no caregiver visibility" and a way to grant tokens as
follow-ups. Both landed here.

**Shape, decided with the user:** a dedicated `adjust_tokens {id, delta_seconds}`
RPC that moves the stored balance, rather than a `token_delta_seconds` field on
`DailyOverride`. A read-time delta fights the spend path — the stored balance
decrements underneath it — and with `carry_over = true` the delta would expire at
midnight while the balance it modified does not. The RPC mirrors how the engine
already earns and spends, works the same for an entry or a `group:<id>`, and is
audit-logged (`AuditEventType::TokensAdjusted`).

**Semantics:** granted time is indistinguishable from earned time. It is capped
by `max_balance_seconds`, spent by the gated activity's sessions, and opens the
gate only once the balance reaches `minimum_seconds` — a grant is not a bypass,
and the availability override remains the tool for "on regardless". Errors are
typed: `Unprocessable` for a subject with no `[tokens]` block (writing a balance
nothing reads is worse than refusing), `NotFound` for an unknown one.

**Visibility:** `EntryView` and `GroupView` gained `tokens: Option<TokenStatus>`
(balance, minimum, unlocked, max_balance, carry_over), so both apps can show how
close a gate is to opening instead of only that it is shut. A member of a
token-gated group carries no gate of its own — the category's is on the
`GroupView`, where the shared budget belongs.

**UI:** an "Earned time" card on the Android entry and category screens with a
±5 min stepper and progress toward the minimum, and an equivalent inline row on
both web cards. `−5 min` disables at a zero balance, `+5 min` at the ceiling.

Verified on the device: a grant over HTTP unlocked a category and its members;
the same grant from the phone moved the balance 300 → 600 s, showed
"Earned time +5m (now 10m).", and wrote the audit row; a −600 s revoke re-locked
the category. Error paths checked over the wire. New coverage: 3 engine tests,
2 dispatch tests, 1 Kotlin wire test.

**Verification gap:** the *web* token row was not visually checked this round —
this box has no Chromium any more, and the Firefox snap won't start inside the
kiosk session (exit 1, snap confinement). It typechecks and renders the same
`TokenStatus` payload confirmed on the wire, but the rendering is unexercised.

## Smaller UI observations

The first three were fixed in a follow-up commit; the last is not a code issue.

- **Only the first reason was rendered** on group rows, entry rows and the entry
  detail screen, so a member blocked by both a cooldown and a spent quota
  revealed the second reason only once the first cleared — which reads like the
  limit moved. A shared `ReasonLines` composable now renders all of them, and
  the web UI's activity card joins them the way its category card already did.
- **The entry detail screen named no category** and offered no way to reach it,
  though the category screen is where a shared limit can be inspected or
  overridden. It now shows a chip that navigates there.
- **The running activity's own row showed "Another activity is running"** — it
  carries `SessionActive` against itself. Suppressed for the in-session row.
  Pre-existing, but rendering every reason made it worse, so it was fixed here
  rather than left as a visible regression.
- **A blocked category read "Up to 0s per session"** — the true cap, but it
  reads as a limit rather than as "not right now". The line is now shown only
  while the category is available; the reasons below carry the message.
- The device chip row showed two identically-named "Pixel 10a" records — a
  duplicate `ShepherdRecord`, unrelated to this branch and not a code fix.

## Notes for the next agent

- `--user shepherd-kiosk` needs `setfacl -m u:shepherd-kiosk:x /home/shepherd-admin`
  once; the repo checkout is otherwise unreachable from a `0750` home.
- Run the daemon **as shepherd-kiosk** for anything involving the phone: the BLE
  claim lives in that user's `admin.toml`, and a dev-runtime data dir would look
  unclaimed to the app.
- The HTTP RPC endpoint is `POST /api/v1/rpc` and it returns the bare result
  value, not a JSON-RPC envelope.
- Back up `~shepherd-kiosk/.local/share/shepherdd/shepherdd.db` before booting a
  fixture against it — fixture sessions write real `usage`, `cooldowns` and
  `token_balances` rows. It was restored from backup at the end of this session.
- A second daemon for isolated store experiments needs a short
  `XDG_RUNTIME_DIR` (e.g. `/tmp/shl`), or the unix socket path exceeds `SUN_LEN`.
- **Killing a daemon can leave its BLE advertisement registered with bluetoothd**,
  and the next start fails with "Failed to register advertisement" — the phone
  then sits on "Disconnected" with nothing wrong on its side. `sudo systemctl
  restart bluetooth` clears it (bonds live in `/var/lib/bluetooth` and survive).
  shepherdd logs the error once and does not retry.

[#5]: https://git.armeafamily.com/albert/shepherd-launcher/issues/5
[#8]: https://git.armeafamily.com/albert/shepherd-launcher/issues/8
