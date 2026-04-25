# Issue 19: Keep screen on while an activity is open

## Goal

The screen timeout (`swayidle` → `swaymsg "output * dpms off"`) should only
fire when no activity is running. While a session is active the display must
stay on, even if the child generates no input (e.g. watching a cut-scene).

## Approach

Sway has a built-in `inhibit_idle` per-window property that tells `swayidle`
not to idle while a matching window is open. Setting it via `for_window` rules
in `sway.conf` is the cleanest Wayland-native solution and requires no changes
to shepherdd.

### Why not change shepherdd?

Shepherdd knows about the *session* lifecycle (process started/ended) but not
about *windows*. The session starts a moment before the activity window
appears. Using `for_window` rules in sway ties inhibition directly to the
window lifetime, which is the correct semantic: the screen must stay on while
the child can see the activity.

When sway releases an `inhibit_idle open` inhibitor (window closed), wlroots
sends a synthetic "activity" event to reset the idle timer. This means the
normal 600-second timeout restarts cleanly after a session ends — no
shepherdd-side timer management is needed.

### Why not a standalone idle-inhibit binary?

A binary using `zwp_idle_inhibit_manager_v1` would need a *mapped* wl_surface
(wlroots only honours inhibitors on mapped surfaces). Creating a mapped surface
without it being visible in the kiosk layout is possible (1×1 SHM buffer behind
a fullscreen window) but adds ~150 lines of `wayland-client` Rust, a new crate,
and extra build dependencies — all for something the compositor already
provides natively.

## Changes

**`sway.conf`** only — no Rust changes.

1. After the general window decoration rules, added two `for_window` rules:

   ```sway
   for_window [app_id="^(?!shepherd-launcher$).*"] inhibit_idle open
   for_window [class=".*"] inhibit_idle open
   ```

   * The `app_id` rule covers all Wayland apps except shepherd-launcher
     (which is always open and must be excluded).
   * The `class` rule covers all Xwayland apps (class is X11-only, so this
     doesn't double-match Wayland apps).
   * shepherd-hud uses wlr-layer-shell and is invisible to `for_window` rules.

2. Added `inhibit_idle none` to each existing Steam scratchpad rule:

   ```sway
   for_window [class="^[Ss]team$"] move scratchpad, inhibit_idle none
   ```

   Steam is preloaded with `-silent` before any session starts; its scratchpad
   windows must not prevent the screen from timing out when the child is away
   from the launcher. The `inhibit_idle none` line overrides the earlier
   `for_window [class=".*"] inhibit_idle open` because `for_window` rules are
   applied in config order and later assignments win.

## Behaviour after the change

| State | inhibit_idle active? | Screen timeout |
|---|---|---|
| Only launcher + HUD visible | No | Normal (600 s idle → DPMS off) |
| Activity window open | Yes | Disabled |
| Steam preload in scratchpad | No | Normal |
| Activity window closes | No (reset) | Restarts from 0 |
