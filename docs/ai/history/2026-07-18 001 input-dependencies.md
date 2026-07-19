# Input dependencies (issue #96)

**Issue:** <https://git.armeafamily.com/albert/shepherd-launcher/issues/96>

> The issue requests a mechanism to make activities contingent on specific input
> types being available: mouse, touch, keyboard, gamepad (and, as future work,
> camera/microphone and MIDI). The motivating example: a "learn to type"
> application installed on a gaming handheld but only displayed when a physical
> keyboard is connected.

## Summary

Activities can now declare `requires_input` — a class (or list) of physical
input devices that must be connected for the activity to be shown and
launchable. `shepherdd` watches `/dev/input` and re-broadcasts availability as
hardware is attached/removed.

```toml
[[entries]]
id = "typing-tutor"
label = "Learn to Type"
requires_input = "keyboard"        # or ["keyboard", "mouse"]  (ALL required)

[entries.kind]
type = "process"
command = "/usr/bin/tuxtype"
```

Supported types: `mouse`, `touch`, `keyboard`, `gamepad`. Camera/microphone and
MIDI are intentionally left out; the enum is closed, so configuring one is a
parse error rather than a silent no-op.

## Design

This is a **gating** condition, not an input-translation behaviour. It is
deliberately kept separate from the pre-existing `input_compat` (which launches
translation sidecars at spawn time and shares the mouse/touch/keyboard/gamepad
vocabulary). The two live in different layers:

- `input_compat` → spawn-time behaviour, threaded through the host adapter.
- `requires_input` → availability gate, evaluated by the core engine alongside
  time windows, quotas, internet, and per-kind readiness.

The implementation mirrors two existing patterns:

- **Internet monitoring** (`shepherdd/src/internet.rs`) — a background task that
  probes an external condition, pushes status into the engine, and broadcasts
  `StateChanged` on change.
- **Per-kind readiness** (issue #76) — an engine-held map consulted in
  `evaluate_entry`, producing a `ReasonCode` when a required capability is
  absent.

### Layers touched

1. **`shepherd-api`** (`types.rs`)
   - New `InputDeviceType` enum (`Mouse`/`Touch`/`Keyboard`/`Gamepad`) with a
     `Display`/`as_str` matching the snake_case config spelling.
   - New `ReasonCode::RequiredInputUnavailable { devices: Vec<InputDeviceType> }`.

2. **`shepherd-config`**
   - `schema.rs`: `RawInputDevice` enum + `requires_input` field on `RawEntry`,
     with a scalar-or-list deserializer mirroring `input_compat`.
   - `policy.rs`: `Entry.requires_input: Vec<InputDeviceType>`, converted +
     sorted + deduplicated in `Entry::from_raw` (sorting keeps the gating reason
     and any UI text stable regardless of config order).

3. **`shepherd-core`** (`engine.rs`)
   - `connected_inputs: Option<HashSet<InputDeviceType>>` on `CoreEngine`.
   - `set_connected_inputs(...) -> bool` setter (returns whether it changed).
   - A gating block in `evaluate_entry` after the internet check, pushing
     `RequiredInputUnavailable` with the sorted missing devices.

4. **`shepherdd`** (`input_devices.rs`, new)
   - `InputMonitor`, spawned from `run()` only when some entry declares
     `requires_input` (zero overhead otherwise — `/dev/input` is never opened).
   - Initial `evdev` enumeration + classification, then hotplug via a `notify`
     watch on `/dev/input` (already a dependency; no new udev dep), plus a slow
     30s fallback re-scan. Scans run on `spawn_blocking`.
   - Classification reuses the bridges' `evdev` heuristics:
     - **mouse** = relative X/Y + `BTN_LEFT`
     - **touch** = `BTN_TOUCH` + absolute X/Y + `INPUT_PROP_DIRECT` (the DIRECT
       check excludes graphics tablets and the absolute "tablet" pointer that
       VMs like QEMU/SPICE expose)
     - **keyboard** = a QWERTY top-row span (`KEY_Q..KEY_Y`), which excludes
       power buttons and consumer-control keys that also report `EV_KEY`
     - **gamepad** = `BTN_SOUTH` (== `BTN_GAMEPAD`) or `BTN_TRIGGER`
       (== `BTN_JOYSTICK`); checking the button rather than a `js` handler keeps
       absolute pointers exposed as `jsN` from counting

5. **`shepherd-launcher-ui`**
   - `client.rs`: `reason_to_message` arm ("Requires an input device").
   - `tile.rs`: `reason_tooltip` names the missing device(s) ("Requires:
     keyboard, mouse"); other reasons keep the prior `Debug` tooltip.

### Fail-open semantics

Availability gating here is a UX convenience, not a security control, so it
fails **open**:

- Before the first scan (`connected_inputs == None`), nothing is considered
  missing — gated activities stay visible.
- A scan that enumerates **zero** devices is treated as "detection unavailable"
  (almost always the daemon lacking `/dev/input` access) rather than "no
  hardware present": `scan_connected_inputs` returns `Option`, the engine set is
  left untouched, and a one-shot warning tells the operator to add the `input`
  group. This avoids silently hiding every gated activity behind a permission
  misconfiguration. A scan that reads ≥1 device is authoritative even if the
  resulting set is empty.

### Permissions

Reading device capabilities needs the daemon's user in the `input` group (the
same requirement the input-compat bridges already have; `/dev/uinput` is not
needed here). Documented in `docs/INSTALL.md`.

## Testing

- `shepherd-config`: scalar/list/absent parse tests, and a test that a future
  type (`camera`) is rejected by the closed enum.
- `shepherd-core`: `test_required_input_gates_show_and_launch` — fail-open
  before first report, gated with the correct reason after a keyboard-less scan,
  no-op on identical report, un-gated once the keyboard appears.
- `shepherdd`: `DeviceCaps::classify_into` unit tests (mouse, direct-vs-indirect
  touch, keyboard, gamepad, combo device, bare button), plus an `#[ignore]`d
  real-hardware smoke test. Verified manually against this machine's real
  `/dev/input`: correctly detects `{Mouse, Keyboard}` and does not miscount the
  SPICE tablet's `js0` node as a gamepad.

## Future work

Camera/microphone and MIDI dependency types (marked future in the issue). Adding
one is a variant on `InputDeviceType`/`RawInputDevice`, a classifier in
`input_devices.rs`, and a UI string — the gating machinery already generalises.
