# shepherd-touch-bridge

Sidecar binary that translates touchscreen input into mouse events for
activities that ignore raw touch events (e.g., *World of Goo*, *Human
Resource Machine*, *7 Billion Humans*).

The bridge is launched by `shepherd-host-linux` when an activity has
`input_compat = "touch_to_mouse"` set in its config. It runs alongside the
activity and exits when terminated.

## How it works

1. Auto-detects every touchscreen device under `/dev/input/event*` (devices
   with `BTN_TOUCH` and an absolute X axis).
2. `EVIOCGRAB`s each one so no other Wayland client receives raw touch
   events while the bridge is running.
3. Connects to the Wayland display, binds `zwlr_virtual_pointer_manager_v1`,
   and creates a virtual pointer.
4. Translates single-touch events:
   - finger down at *(x, y)* → `motion_absolute(x, y)` + `button(BTN_LEFT, pressed)`
   - finger move → `motion_absolute(x', y')`
   - finger up → `button(BTN_LEFT, released)`
5. On `SIGTERM`/`SIGINT`, releases all grabs and exits cleanly.

Multi-touch is intentionally ignored: only the first finger's coordinates
drive the pointer, additional fingers are dropped. This keeps gestures from
producing spurious clicks in the target application.

## Grab-only mode (disable the touchscreen)

With `--grab-only` the bridge performs steps 1–2 (auto-detect and `EVIOCGRAB`
every touchscreen) and then simply **discards** all touch events instead of
translating them. This disables the touchscreen for the lifetime of the
bridge — used by `input_compat = "disable_touch"` (issue #68) for activities
that misbehave on touch input but remain playable with a mouse or gamepad. No
virtual pointer is created, so this mode does **not** require `/dev/uinput`
access (only the `input` group, to grab the devices).

## Permissions

The user running the bridge must be in the `input` group to read
`/dev/input/event*`. The default (translate) mode additionally synthesizes
its pointer through `/dev/uinput`; `--grab-only` does not.

## CLI

```
shepherd-touch-bridge [--device PATH]... [--grab-only]
```

If no `--device` arguments are given, all touchscreens are auto-detected.
Pass one or more `--device` flags to grab specific devices instead.
`--grab-only` grabs the touchscreens and discards their events (no synthetic
output).
