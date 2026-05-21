# Issue 50: Steam activities do not open while offline

<https://git.armeafamily.com/albert/shepherd-launcher/issues/50>

## Background

Steam supports running games offline, but only after the user
acknowledges a one-shot "You appear to be offline — go offline?"
confirmation dialog at client startup. In kiosk mode the launcher is
fullscreen and the Steam main window is suppressed via the
`-silent` flag at preload time plus a defensive
`for_window [class="^[Ss]team$"] move scratchpad` rule in `sway.conf`.
The scratchpad rule was deliberately broad as defense-in-depth in case
the `-silent` preload ever surfaced a window.

Because `class=Steam` matches **every** window Steam maps — main
client, friends/news sub-windows, *and* modal dialogs — the offline
prompt was hidden too. When the network was down at boot, launching a
Steam activity hung indefinitely on an invisible prompt that nothing
in the kiosk could dismiss.

## Scenarios that share the same failure mode

A second case turned up while scoping the fix: the user can be online
when Steam starts (so no offline dialog at startup, preload completes
normally) and lose the network later. When they then launch a game,
Steam attempts a license/Cloud check and pops a *different* dialog
(typically "Connection Error" / "Retry / Go Offline / Cancel"). That
dialog is also `class=Steam`, so the same scratchpad rule swallowed
it.

The real failure mode is "a Steam modal is blocking the launch and
the user can't see it," not "we are offline." The fix targets that
mode directly so it covers both scenarios uniformly.

## Approach

Two layered changes:

1. **Narrow the scratchpad rules to title-only matching**
   (`sway.conf`). The main client has title `"Steam"` (exact) and its
   sub-windows have titles starting with `"Steam - "` (e.g. `"Steam -
   News"`, `"Steam - Friends"`). Unknown dialogs have other titles
   ("Connection Error", "Sign In", etc.), so dropping the class/instance
   rules and keeping only the title rules lets dialogs render while
   still hiding the regular client UI. This alone resolves the most
   common case (boot-offline) without any code changes.

2. **Launch watchdog** in `LinuxHost::start_monitor` (defense in
   depth, and the mechanism for the "lost network mid-session" case
   where a dialog might still slip into the scratchpad via some path
   we missed). Each `SteamSession` now records
   `started_at: Instant`. The existing 100ms polling loop checks
   `find_steam_game_pids(app_id)` — if no game has been seen yet and:
   * elapsed ≥ `STEAM_LAUNCH_SURFACE_TIMEOUT` (60s): scan the sway
     tree for any `class=Steam` window in `__i3_scratch` whose title
     is neither `"Steam"` nor `"Steam - …"` and `swaymsg scratchpad
     show` it. Marked one-shot via `surfaced_dialogs: bool` so we
     don't yank focus repeatedly.
   * elapsed ≥ `STEAM_LAUNCH_ABORT_TIMEOUT` (180s): give up and emit
     `HostEvent::Exited` so the controller tears the session down and
     the kid is returned to the home screen. We don't kill the
     preloaded Steam itself.

The 60s surface threshold is generous: Steam's visible "Preparing to
launch" overlay can legitimately sit for ~30s on a first-launch shader
compile or DRM check, and we don't want to false-positive-surface a
dialog while Steam is doing real work. 180s for abort gives the user
plenty of time to read and dismiss any surfaced dialog before the
session disappears underneath them.

## Approaches considered and rejected

- **Skip the Steam preload when the network is down at boot.** Only
  addresses the boot-offline case; doesn't help when the network
  drops after Steam is already authenticated. Adds a coupling to the
  connectivity-check signal from #49 for marginal benefit. Dropped.
- **Pre-acknowledge offline mode in Steam's `loginusers.vdf`.**
  Fragile (Steam owns the file and may rewrite it), undocumented
  behavior across Steam versions, and doesn't help the first time a
  user goes offline. Dropped.
- **Auto-dismiss the dialog with synthetic input (ydotool/wtype).**
  Brittle: any localization or button-order change in Steam silently
  clicks the wrong control. Dropped in favor of surfacing the dialog
  for the user.

## Files touched

- `sway.conf` — drop the class/instance scratchpad rules; keep the
  title rules; explain why in the comment.
- `crates/shepherd-host-linux/src/sway.rs` — add
  `is_stuck_steam_dialog` (pure predicate, unit-tested) and the
  `surface_stuck_steam_dialogs` async helper that uses the existing
  `list_windows` + `act_on_window` machinery.
- `crates/shepherd-host-linux/src/adapter.rs` — extend `SteamSession`
  with `started_at` / `surfaced_dialogs`, add the two timeout
  constants, and plumb the watchdog into `start_monitor`'s polling
  loop. The async surface call is hoisted out of the mutex-holding
  section so we don't `.await` while holding a `std::sync::Mutex`.

## Verification

- `cargo test --all-targets`: 14 new + existing tests pass, including
  `surfaces_unrecognized_steam_dialogs_only` and
  `steam_class_match_is_case_insensitive` covering the predicate.
- `cargo clippy --all-targets -- -D warnings`: clean.
- `cargo fmt --all`: clean.
- `validate-config` on `config.example.toml`: all 14 entries (including
  the three Steam entries) accepted.
- Live verification of the sway window-title patterns is still
  required on a real install — the predicate matches what Steam is
  *documented* to use, but if Steam ships a build that, say, mints a
  modal with `WM_CLASS = "Steam Dialog"` (different class) we'd want
  to broaden `is_stuck_steam_dialog`'s class match. The watchdog will
  log "no hidden Steam dialog found in scratchpad" in that case,
  which is the actionable signal.
