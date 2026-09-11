# Token gate re-locks below its minimum (#193)

> Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/193>
> Reverses the ratchet from
> [`2026-07-20 001 groups-tokens-companion-validation.md`](2026-07-20%20001%20groups-tokens-companion-validation.md)
> (defect 2, "fix all four, ratchet the gate").

## Prompt

> implement #193

## Issue text

> **Token gate block anytime the time banked is below the minimum**
>
> Consider the following groups:
> * Group A, which can be considered the token source.
> * Group B, which can be considered the token sink. This group has a token gate
>   on group A, and has a minimum token gate.
>
> Then consider this sequence:
> 1. [initial state] Only Group A activities are available.
> 2. Use Group A enough to unlock group B.
> 3. Use Group B until there is less time banked than the minimum token gate.
>
> Currently, the Group B activities stay available until the banked time goes
> to 0.
>
> The desired behavior is for Group B activities to become unavailable until the
> time remaining exceeds the minimum. The intent behind the minimum is to ensure
> that there is enough time banked to actually utilize a Group B activity
> without frustration -- i.e. complete a Pokemon battle, one Outer Wilds loop,
> get a Tetris high score, etc.
>
> The current design does not guarantee this.

## Background

The July validation pass found that a partial spend left a balance below
`minimum_seconds` and so re-locked the gate, "stranding" earned time. Two fixes
were on the table — clamp sessions to `balance - minimum`, or ratchet the gate
open until the balance reached zero — and the ratchet was chosen. It was stored
as a `ratcheted` column on `token_balances`, set by the engine when earning (or a
grant) reached the minimum and cleared by the store when a spend reached zero.

That fixed the stranding by giving up the thing the minimum is for. A gate that
stays open on 40 s of balance hands out 40 s sessions, which is exactly what the
owner set a minimum to prevent.

## Decision

Neither of the July options: the gate is simply `balance > 0 && balance >=
minimum`, checked every time, and **the session clamp stays at the whole
balance**.

- **Why not clamp to `balance - minimum`.** It would cut a session off at the
  threshold, which is the frustration the issue describes, and it would make the
  minimum a reserve that can never be spent.
- **What happens to the remainder.** It stays banked and counts toward opening
  the gate again, so a little more source time reopens it onto the *whole*
  balance. With `carry_over = false` a remainder that is never topped up expires
  at midnight like any other balance. The issue accepts this in so many words
  ("unavailable until the time remaining exceeds the minimum"); it is documented
  in `crates/shepherd-config/README.md` and `config.example.toml`.
- **A running session is never interrupted** at the threshold. Balances only
  move at session end, so there is nothing mid-session to react to anyway.
- **`>=`, not `>`.** The issue says "exceeds", but the minimum has always opened
  the gate on equality, the docs and both apps say "needs 10m", and the
  difference is one second. Left as it was.

## What changed

- `TokensPolicy::unlocked(balance)` lost its `ratcheted` argument.
- The engine no longer records a ratchet after earning or after a caregiver's
  `adjust_tokens` grant.
- `TokenState` lost `ratcheted`; `Store::set_token_ratchet` is gone, from the
  trait, SQLite, and the state custodian's wire.
- **`PROTO_VERSION` 2 → 3.** The protocol's rule is to bump when a meaning
  changes rather than when a variant is added. A removal is not harmless the way
  an addition is: an older client decoding a `TokenState` without `ratcheted`
  would fail on the missing field and treat every gate as locked. The two
  binaries ship together, so this only surfaces as a half-finished upgrade,
  which the handshake refuses loudly.
- **The `ratcheted` column is dropped** from existing databases by a new
  `drop_obsolete_column` migration helper, which replaced `add_missing_column`,
  whose only caller was that column. A stale `1` in a database someone reads by
  hand would otherwise look meaningful. A downgrade is safe: the previous
  release's `add_missing_column` adds the column back with its default.
- The config editor's caption under the token gate, the `TokenStatus.unlocked`
  doc comment (and so the generated Kotlin/TypeScript mirrors), and the store
  README.

The web and Android caregiver UIs needed no code change. Both already show
progress toward the minimum whenever `unlocked` is false, so a partly spent
balance now reads "7m of 10m needed · locked", which is the hint a caregiver
wants.

## Tests

Engine:

- `test_token_gate_relocks_when_a_spend_leaves_less_than_the_minimum` replaces
  the ratchet test. It checks that the gate opens onto the whole balance and
  that a spend leaving 400 s of 600 re-locks the gate with exactly
  `TokensInsufficient { balance: 400 s, required: 600 s }`. It also checks that
  `request_launch` refuses the entry, that the 400 s is still banked, and that
  topping up reopens the gate with a 600 s cap.
- `test_group_token_gate_relocks_every_member_below_the_minimum` covers the
  group-level case the issue was written about. One member's spend locks both
  members.
- `test_manual_grant_banks_time_and_opens_at_the_minimum` (renamed) now also
  checks that granted time spent below the minimum re-locks.

Store: the two ratchet tests are replaced by
`test_obsolete_ratchet_column_is_dropped`. It builds a ratchet-era table with a
`ratcheted = 1` group row, opens it, and asserts that the column is gone, the
balance survived, writes work, and re-opening is a no-op.

## Verification

`cargo test --workspace --all-targets` passed (1362 passed, 0 failed, 6
ignored). `cargo clippy --workspace --all-targets -- -D warnings` and
`cargo fmt --all --check` are clean. `rpc-codegen` was re-run, and the only
mirror changes are the `TokenStatus.unlocked` doc comment. `config.example.toml`
passes `shepherd config validate`, and `npm run typecheck` passes.

End to end, in the headless dev session (`headless-dev` skill), against a
two-entry fixture: `reward` gated on `practice` with `earn_ratio = 1.0` and
`minimum_seconds = 20`. The scenario was driven over `dev-runtime/shepherd.sock`,
with `list_entries` read back after each step:

| Step | `reward` after it | Launcher grid |
|---|---|---|
| start | locked, `tokens_insufficient` 0 / 20 s | Practice only |
| 25 s of `practice` | open, `max_run_if_started_now = 25 s` | Practice, Reward |
| 10 s of `reward` (its deadline was set 25 s out: the whole balance) | **locked, `tokens_insufficient` 15 / 20 s** | Practice only |
| 6 s of `practice` | open, `max_run_if_started_now = 21 s` | Practice, Reward |

Before this change the third row would have stayed open at 15 s. The dev
database's `token_balances` has no `ratcheted` column, and it held `reward = 21`
at the end.

## Notes for the next agent

- **This VM's disk fills up from `target/debug`.** It had grown to 59 GiB of
  the 94 GiB disk, so the test build died with `No space left on device`. That
  reads like a linker failure (`linking with cc failed`) until you look at the
  line above it. `cargo clean --profile dev` freed 62.7 GiB, and a full workspace
  test build put back about 11 GiB. The installed kiosk `shepherdd` on this
  machine shares the disk, so a full disk also fails its database writes.
