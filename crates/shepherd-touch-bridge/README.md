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

## Permissions

The user running the bridge must be in the `input` group to read
`/dev/input/event*`.

## CLI

```
shepherd-touch-bridge [--device PATH]...
```

If no `--device` arguments are given, all touchscreens are auto-detected.
Pass one or more `--device` flags to grab specific devices instead.
