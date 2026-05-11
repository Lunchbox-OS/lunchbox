# Gamepad-to-keyboard+mouse compatibility mode

Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/42>

## Problem

Several gaming handhelds (and any setup where a gamepad is the primary
input) need to use software that ignores raw gamepad input — productivity
applications, non-first-person games, and so on. Without a remapping layer the
user has to bring out a keyboard and mouse.

Issue #42 asks for two preset mappings, building on the per-activity
sidecar pattern introduced for touch-to-mouse in #37/#39:

- **Productivity**: triggers = left mouse button, shoulders = right mouse
  button, left stick = mouse, right stick = scroll, stick-click toggles
  which stick drives the mouse, D-pad = arrow keys, A = Enter, Start =
  Escape.
- **GPD (FPS)**: LT = LMB, RT = RMB, LB = MMB, left stick = WASD, right
  stick = mouse, D-pad = scroll, A = Space, X = R, B = E, Y = F.

## Approach

Mirror the `shepherd-touch-bridge` architecture in a sibling crate
`shepherd-gamepad-bridge`:

1. **Per-activity sidecar** spawned by `shepherd-host-linux/adapter.rs`
   before the activity, with the same lifecycle, cleanup, and `input`
   group requirement.
2. **Gamepad input via gilrs** — the same library `shepherd-launcher-ui`
   already uses for launcher gamepad navigation. gilrs handles
   enumeration, hotplug, and cross-controller button/axis remapping. The
   preset layer consumes `gilrs::Button` and `gilrs::Axis` directly; the
   main loop just pattern-matches `EventType` into
   `state.ingest_button` / `state.ingest_axis` calls. Sign conventions
   follow gilrs (sticks: positive Y = up; D-pad: positive Y = down), and
   the mouse / scroll emitters flip stick Y on the way out so motion
   matches Wayland's screen-coord convention.
3. **Two Wayland outputs**: `zwlr_virtual_pointer_v1` for pointer +
   scroll; `zwp_virtual_keyboard_v1` for synthesized key events. A
   minimal xkb keymap that uses `include "evdev"` / `include "pc+us"` is
   uploaded once via a memfd — the compositor's libxkbcommon resolves
   the includes from the system's xkb data, so the bridge itself doesn't
   depend on libxkbcommon.
4. **125 Hz tick loop**: each frame, drain pending gilrs events into a
   gamepad state snapshot, then translate that snapshot to a list of
   pointer/keyboard `OutputEvent`s per the active preset. Analog →
   relative mouse motion accumulates sub-pixel residue between ticks so
   slow stick deflection still moves the pointer.
5. **Preset selection** via `input_compat` config field — extended from a
   scalar to either a string (back-compat) or list, since touch and
   gamepad presets are orthogonal and may stack on a handheld with both
   input types.

### Why gilrs (and the cost)

gilrs is the project's standard gamepad library (`shepherd-launcher-ui`
already depends on it), so reusing it keeps the dependency surface
small and means the bridge inherits the same controller-quirks
behavior the user already sees in the launcher.

The tradeoff is that gilrs reads `/dev/input/event*` without
`EVIOCGRAB`, so the bridge can't make the activity's own evdev reads
go dark. Activities that read evdev themselves will still receive raw
gamepad input alongside the synthesized mouse + keyboard. For the
target case (activities that ignore gamepads — productivity apps,
non-FPS games) this is fine. For an activity that partially handles
gamepad input, both paths would fire and the user would likely want to
just leave the bridge off via config.

### Why a separate binary

The touch bridge stays mouse-only and depends only on
`wayland-protocols-wlr`; the gamepad bridge needs
`wayland-protocols-misc` for the virtual-keyboard protocol and pulls in
the keymap-upload path. Splitting keeps the touch bridge minimal.

### Why a polling tick instead of event-driven

evdev only emits an `ABS_*` event when the stick crosses a kernel
threshold. With pure event-driven dispatch, holding the stick at a
constant deflection produces zero events and therefore zero pointer
motion. A fixed-cadence tick samples current deflection and converts it
to motion regardless of whether new events arrived.

### Config schema

`input_compat` is now a list of orthogonal modes. The deserializer also
accepts a scalar for compatibility with existing `input_compat =
"touch_to_mouse"` configs. Conflicting gamepad presets are dropped at
policy-conversion time with a warning; duplicates are silently
deduplicated. Tunables live in a parallel `input_compat_options` block
so each gamepad-using activity can adjust deadzone / mouse speed /
scroll speed without affecting others.

```toml
[[entries]]
input_compat = ["touch_to_mouse", "gamepad_productivity"]

[entries.input_compat_options]
gamepad_deadzone = 0.15
gamepad_mouse_speed = 800.0
gamepad_scroll_speed = 10.0
```

### Preset modeling

`PresetState` is a plain state machine — no I/O — driven each tick by
the current `GamepadState` snapshot. It stores `gilrs::Button` and
`gilrs::Axis` directly in `HashMap`s; output is a `Vec<OutputEvent>` of
pointer/keyboard intents. Tests poke state via `ingest_button` /
`ingest_axis` and assert on the produced events — no Wayland connection
or controller needed. 16 tests cover deadzone math, sub-pixel
accumulation, both presets' button mappings, sign conventions
(stick-up → mouse-up, D-pad-up → arrow-up), the productivity-mode
toggle (edge-triggered on thumb-click press, doesn't flip on hold),
and shutdown release of held inputs.

## What was *not* implemented

- **Tunable mouse curve.** Linear deflection-to-velocity only; no
  exponential / cubic / dead+linear curves. Easy to add later if the
  defaults feel sluggish or twitchy.
- **Per-preset tunable overrides.** `input_compat_options` applies to
  whichever preset is configured; the productivity and GPD presets share
  the same deadzone/mouse/scroll fields.
- **Modifier-key emission.** All mapped keys are unmodified
  (W/A/S/D/Space/Enter/Escape/Arrows/E/R/F). If a future preset needs a
  modified key, the bridge would need to send `modifiers()` events
  alongside `key()`.
- **Right-click on two-finger taps / runtime preset switch.** Out of
  scope for #42; revisit if needed.
- **Multi-controller arbitration.** If multiple gamepads are connected,
  events from all of them merge into a single `GamepadState`. The
  last-write-wins for axes; this is fine for typical setups (one player,
  one controller).

## System requirements

- The user running `shepherdd` must be in the `input` group to read
  `/dev/input/event*` (gilrs needs the same access). Already documented
  in `docs/INSTALL.md`.
- Compositor must support `zwlr_virtual_pointer_v1` (required since #39)
  and `zwp_virtual_keyboard_v1` (Sway / wlroots). If the keyboard
  protocol is missing the bridge logs a warning and drops keyboard
  events but continues to emit pointer events.

## Files touched

- `crates/shepherd-api/src/types.rs` — extend `InputCompatMode` and add
  `InputCompatOptions`
- `crates/shepherd-config/src/schema.rs` — `RawInputCompat` variants,
  `RawInputCompatOptions`, scalar-or-list deserializer for `input_compat`
- `crates/shepherd-config/src/policy.rs` — list conversion + conflict
  resolution
- `crates/shepherd-host-api/src/traits.rs` — `SpawnOptions::input_compat`
  to `Vec` + new `input_compat_options`
- `crates/shepherd-host-linux/src/sidecar.rs` — generic
  `sidecar_binary` lookup + `spawn_gamepad_bridge` with preset CLI
- `crates/shepherd-host-linux/src/adapter.rs` — iterate `input_compat`
  list, spawn the appropriate sidecar per mode
- `crates/shepherd-gamepad-bridge/` — new crate
- `crates/shepherdd/src/main.rs`,
  `crates/shepherd-http/src/handlers/sessions.rs` — plumb the new fields
  through `SpawnOptions`
- `config.example.toml`, `docs/INSTALL.md` — examples and notes
- `Cargo.toml` — register new crate

## Conversation context

Built from the design discussion summarized in the corresponding PR
description. The user picked: ship both presets in v1, allow stacking
with `touch_to_mouse` (via list-valued `input_compat`), and expose
tunables per-activity via `input_compat_options`. Implementation order:
schema → sidecar skeleton → preset mappings → adapter wiring → docs.
