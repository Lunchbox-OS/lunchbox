# Per-activity-type readiness signal, implemented for Steam (issue #76)

## Prompt

Issue #76 (<https://git.armeafamily.com/albert/shepherd-launcher/issues/76>),
titled "Hide Steam activities until Steam has finished its initial load", body:

> It may make sense to generalize this to an activity readiness state (rather
> than something specific to Steam) that is consumed when we are determining
> whether the activity can be showed or launched.

The agent was asked to "Add a per-activity-type readiness signal, then implement
it for Steam."

Two decisions were confirmed with the maintainer up front:

1. **Steam-ready detection**: presence of the `steamwebhelper` process, plus a
   fallback timeout so a missed signal never hides games forever.
2. **UI behaviour when not ready**: the activity is **fully hidden** from the
   launcher grid (which already omits `!enabled` entries), reappearing once
   ready.

## Design

Readiness is host-provided state, exactly like internet connectivity — so it is
plumbed through the same three layers the `internet_status` gate already uses:
host detection → `CoreEngine` state → `evaluate_entry` reason → UI.

- **Definition.** A new blocking `ReasonCode::NotReady { kind }`
  (`shepherd-api`). Kinds are keyed by the existing lightweight `EntryKindTag`,
  so readiness is genuinely per-activity-*type*, not per-entry.
- **Engine state.** `CoreEngine` holds `kind_readiness: HashMap<EntryKindTag,
  bool>`. A kind **absent** from the map is treated as ready (kinds with no
  warm-up concept are never gated); a kind mapped to `false` is warming up.
  `set_kind_readiness(kind, ready) -> bool` mirrors `set_internet_status`
  (returns whether the stored value changed). `evaluate_entry` gates right after
  the host-capability check: not-ready ⇒ `enabled = false` + `NotReady`. This is
  the single chokepoint used for both `list_entries` (show) and `request_launch`
  (launch), so one gate covers "shown or launched".
- **Signal transport.** A new `HostEvent::KindReadinessChanged { kind, ready }`.
  The daemon's host-event loop applies it to the engine and, on change,
  re-broadcasts a `StateChanged` snapshot so the launcher refreshes.
- **UI.** No launcher change was needed: `LauncherGrid::set_entries` already
  skips `!entry.enabled`, so a `NotReady` gate hides the tile and it reappears
  on the next snapshot. `reason_to_message` gained a "Still starting up" string
  for the admin/HTTP surfaces that render reasons.

### Steam implementation

- **Detection** (`process::steam_webhelper_running`): scans `/proc/*/comm` for
  Steam's `steamwebhelper` (its Chromium UI/service backend), which must be up
  for the modern client to launch a game. Works regardless of whether the CEF
  remote-debugging endpoint is enabled (unlike the interstitial machinery). The
  comm match is factored into a pure `comm_is_steamwebhelper` helper — `comm` is
  truncated to 15 chars and "steamwebhelper" is 14, so an exact match is right.
- **Watcher** (`LinuxHost::spawn_steam_readiness_watcher`, started from
  `preload_steam`): emits `{Steam, false}` immediately, then `{Steam, true}`
  once `steamwebhelper` appears — or after `STEAM_READY_FALLBACK` (120 s), a
  safety net so a missed signal or failed preload never leaves games hidden.
  Started unconditionally (even if the Steam spawn fails) for that reason.
- **Boot seeding** (`shepherdd`): when Steam entries exist, the engine is seeded
  `set_kind_readiness(Steam, false)` before the first snapshot is served, so
  Steam is gated from the very first paint (no flash of clickable tiles). The
  watcher then flips it to ready.

Gating is automatic — no new config. The 120 s fallback is the guard against a
bad heuristic; there is intentionally no way to make Steam stay hidden forever.

## What was implemented

- `crates/shepherd-api/src/types.rs` — `ReasonCode::NotReady { kind }`.
- `crates/shepherd-host-api/src/traits.rs` — `HostEvent::KindReadinessChanged`.
- `crates/shepherd-core/src/engine.rs` — `kind_readiness` map,
  `set_kind_readiness`, `kind_ready`, gate in `evaluate_entry`; unit test
  `test_kind_readiness_gates_show_and_launch`.
- `crates/shepherd-host-linux/src/process.rs` — `steam_webhelper_running` +
  `comm_is_steamwebhelper`; tests.
- `crates/shepherd-host-linux/src/adapter.rs` — `STEAM_READY_FALLBACK`,
  `spawn_steam_readiness_watcher`, hooked into `preload_steam`.
- `crates/shepherdd/src/main.rs` — boot seeding + `KindReadinessChanged`
  handler.
- `crates/shepherd-launcher-ui/src/client.rs` — `NotReady` reason string.

## Notes / limitations

- Readiness is monotonic within a daemon lifetime: once Steam reports ready the
  watcher stops. If the preloaded Steam later dies, games stay shown (launching
  just re-warms Steam) — no worse than before this change.
- `reload_policy` intentionally preserves readiness (host state, not policy).
  As before, config reload does not re-trigger Steam preload.

Verified: `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, and
`cargo test --all-targets` all clean.
