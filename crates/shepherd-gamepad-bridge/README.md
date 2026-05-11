# shepherd-gamepad-bridge

Sidecar binary that translates gamepad input into mouse + keyboard events
for activities that ignore raw gamepad input, or for which gamepad input
doesn't fit the activity's interaction model (productivity software,
non-first-person games, etc.).

The bridge is launched by `shepherd-host-linux` when an activity has a
`gamepad_*` value in its `input_compat` list. It runs alongside the
activity and exits when terminated.

## Presets

### `productivity`

Targets non-game / point-and-click activities.

| Input                       | Action                       |
| --------------------------- | ---------------------------- |
| Left trigger / Right trigger| Left mouse button            |
| Left bumper / Right bumper  | Right mouse button           |
| Left stick                  | Move mouse (default)         |
| Right stick                 | Scroll (default)             |
| L3 / R3 (stick click)       | Swap which stick moves vs scrolls |
| D-pad                       | Arrow keys                   |
| Start                       | Escape                       |
| A (south)                   | Enter                        |

### `gpd`

Modeled after the mouse mode on GPD handhelds; suits first-person and similar
games.

| Input              | Action            |
| ------------------ | ----------------- |
| Left trigger       | Left mouse button |
| Right trigger      | Right mouse button|
| Left bumper        | Middle mouse button|
| Left stick         | WASD              |
| Right stick        | Move mouse        |
| D-pad              | Scroll            |
| A (south)          | Space             |
| B (east)           | E                 |
| X (north)          | R                 |
| Y (west)           | F                 |

## How it works

1. Uses [gilrs](https://docs.rs/gilrs) for gamepad enumeration and event
   reading — the same library `shepherd-launcher-ui` uses for the
   launcher's gamepad navigation. gilrs handles hotplug, cross-controller
   button/axis remapping, and SDL-style standardization.
2. Binds `zwlr_virtual_pointer_v1` + `zwp_virtual_keyboard_v1`, uploads a
   US/evdev xkb keymap.
3. Runs a 125 Hz tick loop:
   - Pumps every pending gilrs event into a state snapshot; gilrs sends
     us already-normalized analog values and translated buttons.
   - Emits press/release for any changed buttons (per the active preset).
   - Converts current stick deflection (with deadzone + linear curve) to
     per-tick relative mouse motion, scroll, or WASD held-state, depending
     on the preset.
4. On `SIGTERM`/`SIGINT`, releases everything still latched as held and
   exits cleanly so the compositor doesn't see a phantom key/button.

gilrs reads `/dev/input/event*` directly without `EVIOCGRAB`, so
activities reading evdev themselves will still see raw gamepad input
alongside the synthesized mouse + keyboard. That's fine for the target
case (activities that ignore gamepads) and matches the rest of the
project's gamepad story.

## Permissions

The user running the bridge must be in the `input` group to read
`/dev/input/event*`.

## CLI

```
shepherd-gamepad-bridge --preset (productivity|gpd) [options]
```

Options:

- `--deadzone <FLOAT>` — stick deadzone (0..1, default 0.15)
- `--mouse-speed <FLOAT>` — pixels per second at full deflection (default 800)
- `--scroll-speed <FLOAT>` — wheel units per second at full deflection (default 10)
