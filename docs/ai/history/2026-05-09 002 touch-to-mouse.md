# Touch-to-mouse compatibility mode

Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/37>

## Problem

Several activities (notably *World of Goo*, *Human Resource Machine*, and
*7 Billion Humans*) ignore raw Wayland/X11 touch events. They are otherwise
well suited to a touchscreen — they were originally designed around mouse
input, and the input feels natural with a finger.

We want a per-activity compatibility mode that translates touch into mouse
so these games are playable on a touchscreen, without affecting activities
that already handle touch correctly (e.g., GCompris, kid-paint).

## Approach

Per-activity sidecar daemon that operates entirely at the host/compositor
layer:

1. When an activity is launched with `input_compat = "touch_to_mouse"`,
   `LinuxHost::spawn` co-spawns a sibling `shepherd-touch-bridge` process.
2. The sidecar discovers all touchscreen devices via `/dev/input/event*`,
   `EVIOCGRAB`s each one (so no other Wayland client sees the raw touch
   events), and listens for single-touch events (`BTN_TOUCH` + `ABS_X`/`Y`).
3. It connects to the same Wayland display as the activity and binds
   `zwlr_virtual_pointer_manager_v1` (Sway natively supports this).
4. Each touch is translated into `motion_absolute` + `BTN_LEFT` press/release
   events on the virtual pointer. Sway dispatches these to the focused
   surface — i.e., the activity — exactly as if a real mouse moved/clicked.
5. When the activity exits, the sidecar is terminated alongside it; it
   releases the grab on drop, restoring touch to the launcher.

### Why this design

- **Type-agnostic.** Works identically across `Process`, `Snap`, `Steam`,
  `Flatpak`, `Vm`, and `Media` activity kinds because the sidecar doesn't
  touch the activity's process, sandbox, or rendering pipeline. The sandbox
  sees only synthesized pointer events from the compositor.
- **No `/dev/uinput`.** The Wayland virtual-pointer protocol means we don't
  need root or a uinput udev rule.
- **Exclusive grab.** Activities that *do* handle touch (e.g., other games,
  the launcher home screen) are unaffected, because the grab only exists
  while the touch-compat activity is running.
- **No daemon-level Wayland client.** Keeping Wayland code out of `shepherdd`
  means the daemon doesn't need a display connection of its own. Each
  sidecar is short-lived and bound to one activity.

### Per-type notes

| Kind | Notes |
| ---- | ----- |
| Process | Direct case |
| Snap | Sidecar runs on host (outside snap sandbox); snap'd app receives synthesized pointer events normally |
| Steam | Same as Snap (Steam runs inside its own snap) |
| Flatpak | Same — sidecar on host, Flatpak's Wayland socket forwards pointer events |
| Vm | Host sidecar synthesizes pointer events; VM viewer translates to guest mouse if the viewer supports it. Won't help if guest software ignores its own touch. |
| Media | Generally unnecessary (most media players already handle touch) but supported for completeness |

## What was *not* implemented

- **Multi-touch / right-click.** Single-finger only for now. Could extend to
  emit `BTN_RIGHT` on a two-finger tap if a future activity needs it.
- **Gesture recognition.** No drag thresholds, tap timing, or scroll
  emulation. Touch motion → mouse motion 1:1.
- **Per-output mapping.** The virtual pointer is unbound (no
  `motion_absolute` x_extent/y_extent specific to a particular output). On
  multi-monitor setups, the touchscreen's coordinate range maps to the
  full compositor space; on a single-output kiosk this is correct.
- **`SDL_TOUCH_MOUSE_EVENTS`.** Mentioned as approach #1 in the design
  conversation; not used because the named games don't use SDL. Users can
  still set this via the existing per-activity `env` field if useful.

## System requirements

- The user running `shepherdd` must be a member of the `input` group to
  read `/dev/input/event*`. Documented in `docs/INSTALL.md`.
- Sway must support `zwlr_virtual_pointer_v1` — true on all reasonably
  modern wlroots compositors and required already by the project's Wayland
  baseline.

## Files touched

- `crates/shepherd-config/src/schema.rs` — `RawInputCompat`, `RawEntry::input_compat`
- `crates/shepherd-config/src/policy.rs` — mirror on validated `Entry`
- `crates/shepherd-api/src/types.rs` — shared `InputCompatMode` enum
- `crates/shepherd-host-api/src/traits.rs` — `SpawnOptions::input_compat`
- `crates/shepherdd/src/main.rs` — populate `input_compat` in spawn options
- `crates/shepherd-host-linux/src/adapter.rs` — sidecar spawn/cleanup
- `crates/shepherd-touch-bridge/` — new crate, sidecar binary
- `config.example.toml` — example
- `docs/INSTALL.md` — input-group note
- `Cargo.toml` — add new crate to workspace

## Conversation context

This document was written based on the design discussion summarized in the
PR description; the original prompt asked for viable approaches, the chosen
approach (#2) was the per-activity Wayland virtual-pointer sidecar.
