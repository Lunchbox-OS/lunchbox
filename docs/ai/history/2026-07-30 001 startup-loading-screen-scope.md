# Scope: show the loading screen while Android preboots (#2)

<https://git.armeafamily.com/albert/shepherd-launcher/issues/2>

Scope only — no code landed. Written as a handoff after the branch's Android work
was otherwise merge-ready (through `24561f3`).

## Prompt

> hm let's put up the loading screen until the signal says we're ready

Following a discussion of what remained before review, where the boot-time
scale-1 window was the last user-visible rough edge.

## Why

Preboot boots Waydroid with every output held at scale 1, because Android derives
its display geometry from the scale it observes at session boot and cannot be
corrected warm. During that hold the launcher grid and HUD are still on screen
and render at scale 1 — physically smaller than normal.

Measured on the rig (cold container, config-time `output * scale 1.5`, 1920x1080):

| marker | timing |
|---|---|
| scale-1 hold (the visible artifact) | **~15s** (15–23s across runs) |
| preboot start -> pre-boot complete | ~65s cold, 4–6s warm |
| preboot start -> Android un-gated | ~67s cold |

The hold is already down from ~65s (it used to span the whole boot). The
remaining ~15s is the hwcomposer's first `waydroid.display_scale` write, which is
the real signal that Android has taken the scale. Shortening it further is
possible but is a separate tuning question — see "Related" below.

Covering the window with the launcher's existing loading page removes the
artifact regardless of how long the hold ends up being.

## What already exists

- **`LinuxHost::waydroid_preboot_done`** (`shepherd-host-linux/src/adapter.rs`) —
  `AtomicBool`, starts `true`, set `false` synchronously before the preboot task
  spawns and `true` on **every** exit path. Android readiness is already gated on
  it. This is the state the UI needs; it just has no route to the launcher.
- **The launcher's loading page** — `stack.set_visible_child_name("loading")` in
  `shepherd-launcher-ui/src/app.rs`, already used by `LauncherState::Launching`
  and the connecting path. Nothing new to draw.
- **`LauncherState`** (`shepherd-launcher-ui/src/state.rs`) — `Connecting`,
  `Idle`, `Launching`, `SessionActive`, `Error`, `Suspending`. Driven by
  `handle_event`.

## Proposed change

1. **`HostEvent::StartupBusy { busy: bool }`** — emitted by `preboot_waydroid` at
   entry and on every exit path, alongside the existing `waydroid_preboot_done`
   store. Keep the two in lockstep; they answer the same question.
2. **A new `EventPayload` variant** in `shepherd-api/src/events.rs`, broadcast by
   shepherdd's `HostEvent` handler. **This is a wire type**: regenerate with
   `cargo run -p shepherd-wire-codegen --bin rpc-codegen` (updates
   `docs/rpc-schema.json`, the TypeScript, and `WireTypes.generated.kt`). The
   drift test now runs in CI (`3a5924c`), so a missed regen fails the build
   rather than shipping stale generated files.
3. **Launcher**: a state that renders the loading page while busy, returning to
   `Idle` when it clears. Must not clobber `SessionActive` — preboot and a live
   session shouldn't overlap, but the transition should be explicit rather than
   assumed.
4. **HUD**: decide (see below).

## Open decisions

- **Does the HUD show a startup state too?** With the grid replaced by a loading
  page, a fully-functional HUD above it reads oddly. Options: leave it, blank it,
  or give it a matching "starting up" state.
- **Generic or Waydroid-specific?** A `StartupBusy` covering *any* slow startup
  work is barely harder than a Waydroid-only one and would also cover the Steam
  preload, which has the same shape. Naming and scope differ; the plumbing does
  not. Preferred, unless there's a reason to keep it narrow.

## Verification

On the headless rig (see the `headless-dev` skill):

1. Cold container, config **with** Android entries: the loading page is up for
   the duration of the scale-1 hold, and the grid returns after `pre-boot
   complete`.
2. Config with **no** Android entries: the grid appears normally.
   `waydroid_preboot_done` starting `true` should already cover this, but it is
   the easy thing to break and cheap to check.
3. `[service.waydroid] preboot = false`: same as (2).
4. Preboot **failure** path (e.g. stop the container mid-boot): the loading page
   must clear. This is why the event has to fire on every exit path, not just the
   success one.

## Gotchas

- Emit on **every** preboot exit path. A missed path leaves the kiosk showing a
  loading screen forever — strictly worse than the artifact being fixed.
- Set busy **before** the task spawns, mirroring how `waydroid_preboot_done` is
  handled, so the UI can't observe the gap between "session up" and "preboot
  started".
- Don't gate this on Android *readiness* — that flips ~2s before preboot
  completes and would uncover the window it is meant to hide.

## Related

The scale-1 hold could likely be shortened from ~15s toward ~6s: an earlier
timer-based version held ~6s and still produced correct geometry, suggesting the
geometry may come from the pinned resolution props rather than the hwcomposer's
scale read. That experiment is now *safe* to run, because `24561f3` verifies
Android's display size against the panel's physical mode after preboot and drops
the native-scale claim on a mismatch — so a too-early restore self-corrects
instead of silently rendering wrong. Independent of this scope; either change
stands alone.
