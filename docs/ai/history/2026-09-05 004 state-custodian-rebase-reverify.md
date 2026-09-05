# State custodian — rebase onto main and re-verify (issue #157, PR #161)

Prompt: "#161 is currently checked out. I am planning on merging it right after
#176. Rebase and reverify, then release the box so I can perform manual testing
with a real session".

PR #161 branched at `c046a7d` (#169, media-refresh). Main had since taken #173
(pause the clock on sleep), #174 (HUD on the side), and #175 (generate the config
editor's defaults), so the branch was four merges behind. This note records the
rebase onto `e75344d` and the re-run of every gate.

## The rebase

All 12 commits replayed onto `e75344d` with no conflicts, and the branch's own
patch was diffed before vs. after: the same file set, with only blob hashes and
hunk offsets moved. Nothing was re-merged by hand.

## What the rebase actually changed: a new enum #161 had never seen

The one substantive delta came from the codegen, not from git. This branch's
`fix(codegen): let the companion survive an enum value it predates` teaches the
Kotlin generator to give **every** wire enum a tolerant decoder — a `wire` string,
an `UNKNOWN` fallback, and a custom `KSerializer` — so a companion build does not
fail the decode of everything around an enum value a newer device sends.

Main's #171 then added `HudOrientation`, an enum that did not exist when #161
branched. Git merged both cleanly, because they never touch the same lines: #161
changes `kotlin_types.rs`, #171 adds a type in `shepherd-api`. The *product* of
the two is what drifted — the checked-in `WireTypes.generated.kt` still carried
`HudOrientation` in main's plain `@SerialName` form, which is precisely the shape
#161 exists to abolish.

`codegen_outputs_match_checked_in` caught it. Regenerating with

```sh
cargo run -p shepherd-wire-codegen --bin rpc-codegen
```

produced exactly one changed file, and only `HudOrientation` inside it, now with
the `UNKNOWN("__unknown")` fallback and serializer every other wire enum on this
branch has. That regeneration was folded into the codegen commit rather than left
as a follow-up: it is the same mechanism applied to one more enum, not a new
decision.

Worth keeping in mind for the next rebase of a codegen-shaped branch — a green
`git rebase` says nothing about whether the generated files still match what the
generator would now emit. The drift test is the thing that knows.

## Static gates (post-rebase)

| Gate | Result |
| --- | --- |
| `cargo test --workspace --all-targets` | 1178 passed, 0 failed, 23 ignored |
| `cargo clippy --workspace --all-targets -- -D warnings` | clean |
| `cargo fmt --all -- --check` | clean |
| `cargo test -p shepherd-e2e -- --include-ignored` | 17 passed, 0 failed |
| `:app:testDebugUnitTest` | BUILD SUCCESSFUL (the regenerated Kotlin compiles) |
| `npm test` / `check:boundary` / `check:coverage` | 83 passed; boundary clean; 139/139 fields reachable |
| `shepherd version check` | 0.4.1 everywhere |
| `validate-config config.example.toml` | passes |
| `shellcheck`, `check-arch-neutral.sh`, `check-workflows.sh` | clean |

The test run set `SHEPHERD_REQUIRE_PEER_CGROUP=1`, as CI does, so the
peer-cgroup tests this branch touches turned a skip into a failure rather than
passing silently.

## Composed with #176, which merges first

#176 (bill a session to the day it started) is queued ahead of this branch, so
the pair was checked together rather than only against current main: #161 merged
onto `fix/170-bill-usage-to-the-start-day` with no conflict, and the combined
tree passed the full suite (1178), clippy, and e2e.

No file overlaps — #176 is `shepherd-core/engine.rs`, #161 is
`shepherd-store/src/traits.rs` and outward. The seam worth checking was semantic:
#176 makes the billed day `session.started_at.date_naive()`, and #161 moves the
store that receives it behind an RPC. It holds because the day is a *parameter*,
not something the store derives — `add_usage(entry_id, day, duration)` — and
`RemoteStore::add_usage` forwards `day` verbatim in `StateRequest::AddUsage`. A
store that computed the day itself would have made these two PRs a real
collision; this one cannot.

## End-to-end, in the headless session

`dev headless --no-build` on the rebased branch: launcher grid maps and paints,
HUD along the top (#171's default, undisturbed), `health` reports
`policy_loaded`, `store_ok`, `ready`. A `launch` of `tuxmath` over the daemon
socket was approved with a deadline and a session id, and the session's usage
landed in the store keyed by day (`tuxmath`, `2026-09-05`) — the write path #161
relocates and #176 re-keys.

**The custodian itself is deliberately not exercised here.** Every dev entry
point passes `--no-state-custodian` (`headless.sh` fails loudly if `sway.conf`
stops doing so), because a dev box has no `shepherd-stated` installed and no
kiosk session for one to trust. Confirming that `shepherdd` reads policy and
state *through* the custodian, and that the migration runs on an installed
device, needs a real session — which is what the box was released for.
