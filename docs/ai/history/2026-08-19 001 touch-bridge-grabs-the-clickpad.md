# Touch-to-mouse maps a corner of the panel to the whole screen — the bridge grabs the clickpad too

Reported against release 0.3.7 on `copernicus` (Legion Go S): with
`input_compat = "touch_to_mouse"`, roughly the top-left quarter of the panel
covered the entire screen, and touches past it did nothing. Phrased as
"again", because it looks like a return of issue #47 (touch-compat coordinate
offset) — it is not. Different cause entirely.

## Symptom

Touch-to-mouse cursor motion is amplified and then dead: sliding left to right
across the panel drove the cursor across the whole screen within the first
~21% of the width, then stopped moving.

Notably, it appeared **after switching to a regular GNOME session on a
different user and back** — nothing in shepherd changed, and the same release
had worked earlier the same evening.

## Root cause

Two defects in `shepherd-touch-bridge` compounding:

1. **`looks_like_touchscreen()` matched the touchpad.** It tested only for
   `BTN_TOUCH` plus absolute X/Y axes. Clickpads report exactly that, so the
   Legion Go S touchpad (`INPUT_PROP_POINTER | INPUT_PROP_BUTTONPAD`, range
   `0..400`) was auto-detected and `EVIOCGRAB`'d alongside the real
   `0..1920 x 0..1200` panel (`INPUT_PROP_DIRECT`).

2. **One shared `DeviceRange` normalized every device's coordinates.** `main`
   kept the range of *the first device that opened* and passed it to
   `emit_update` for all readers. Reader threads sent raw device coordinates.

So the panel's raw coordinates were normalized against the touchpad's
`0..400`: `400/1920 = 20.8%` of the width and `400/1200 = 33%` of the height
mapped to the full screen, and `normalize_axis`'s clamp pinned everything past
that at the edge.

### Why the session switch triggered it

`evdev::enumerate()` is a bare `std::fs::read_dir("/dev/input")` with **no
sorting** (evdev 0.13.2, `raw_stream.rs:736`), so which matching device comes
first is arbitrary readdir order. The Legion Go S is USB-attached; the session
switch re-probed it and its touchpad node moved from `event6` to `event10`.
Before, the panel happened to open first and its range was the one kept —
correct by luck. After, the touchpad won.

This is visible directly in the journal, one line per grabbed device:

```
# working, earlier the same evening
Grabbed touchscreen path=/dev/input/event7  name=NVTK0603:00 0603:F200       x_range="0..1920" y_range="0..1200"
Grabbed touchscreen path=/dev/input/event6  name=wch.cn Legion Go S Touchpad x_range="0..400"  y_range="0..400"

# broken, after the session switch
Grabbed touchscreen path=/dev/input/event10 name=wch.cn Legion Go S Touchpad x_range="0..400"  y_range="0..400"
Grabbed touchscreen path=/dev/input/event7  name=NVTK0603:00 0603:F200       x_range="0..1920" y_range="0..1200"
```

**Any recurrence of a coordinate-mapping bug should start with these lines.**
More than one `Grabbed touchscreen` line is itself the bug signal.

## How it was diagnosed (live repro)

`evtest` on the bridge's own synthetic device — found via
`grep -A5 "shepherd-bridge virtual absolute pointer" /proc/bus/input/devices`
— is what settled it, because it shows exactly what the bridge emits before
any compositor mapping. The device is not grabbed, so this works while the
bridge runs:

```
ABS_X value 0 → 164 → 328 → 655 → 983 → … → 65535, then no further events
```

Two numbers fall out and identify the bug on their own:

- The step is **164 counts per raw unit**, and `65535/400 = 163.84` — so the
  divisor in use is 400, the *touchpad's* extent, not the panel's 1920.
- It **pins** at 65535 (`rescale_abs` clamps) rather than wrapping. An earlier
  report of "wrapping back to 0" was a new stroke starting at the left edge,
  not an overflow; worth knowing that the two look alike in an `evtest` scroll.

A red herring worth recording: the touchpad **kept working normally** during
the repro, which seems to disprove the grab. It does not — the Legion Go S
exposes that USB interface (`5-1:1.2`, HID `0003:1A86:E310.000A`) as *two*
nodes, an absolute one (`event10`, grabbed) and a relative "Mouse" one
(`event4`, untouched). Relative motion flows through the node the bridge never
takes.

## Fix

- `shepherd-touch-bridge`: `is_direct_touchscreen()` (a pure function over a
  `TouchCaps` struct, so the rule is unit-testable without a real device node)
  now also requires the device to be *direct*: `INPUT_PROP_DIRECT`, or
  claiming neither `INPUT_PROP_POINTER` nor `BTN_TOOL_FINGER`. That mirrors
  how udev's `input_id` builtin classifies these devices, and the fallback
  keeps panels that omit the property working.
- `shepherd-touch-bridge` and `shepherd-tablet-bridge`: `TouchUpdate` now
  carries coordinates **already normalized against the emitting device's own
  range**, plus its extents. The reader thread already had its device's range;
  it just wasn't using it. The shared `device_range` in `main` is gone, along
  with the "first device that opens" heuristic and the `no usable device
  range` error.
- Rejected devices are logged at `debug` during discovery, so the next
  occurrence of this class of bug is one `RUST_LOG=debug` away.

The tablet bridge carried an identical copy of defect 2 (not defect 1 — it
already excludes `INPUT_PROP_DIRECT`, since it re-maps *indirect* devices).
Fixed the same way, but it has **no live repro** — no tablet hardware handy —
so `tablet_to_touch` still wants re-testing on real hardware.

Each fix is independently sufficient for this repro; both are worth having.
Filtering keeps the touchpad usable during a `touch_to_mouse` activity (it was
being grabbed and silently swallowed), and per-device normalization makes the
enumeration-order dependence structurally impossible rather than merely
unlikely.

## Verified

- `cargo test` green for the touched crates (12 tests in touch-bridge, 5 in
  bridge, 46 in host-linux, 2 in tablet-bridge); `cargo clippy --workspace
  --all-targets` clean; `cargo fmt --all`.
- New tests cover the classifier (direct panel accepted, panel without the
  property accepted, clickpad rejected, `INPUT_PROP_DIRECT` overriding the
  indirect hints, button/axis bits still required) and per-device
  normalization, including the exact `0..400`-vs-`0..1920` saturation this bug
  produced.
- **Not yet re-tested on the hardware**: needs a `touch_to_mouse` activity on
  `copernicus` with the touchpad present. Expect exactly one `Grabbed
  touchscreen` line, naming `event7`.
