# Tablet-to-touch input-compat sidecar

## Prompt

The user asked, in sequence:

1. Whether the existing touch and gamepad sidecars actually *consume* input
   (grab) so the originals never reach the activity.
2. To sketch an inverse of the touch→mouse bridge: a **mouse/pen → touch**
   sidecar, for developing upstream touch patches to activities and for
   nonstandard hardware that reports mouse/pen events when touch would be more
   appropriate.
3. Whether this environment's input stack (SPICE/QEMU virtual tablet) needs
   special consideration.
4. To **implement a tablet→touch interface only**, referencing this hardware
   but also working with real tablets.

## Findings that shaped the design

### Existing sidecars (consume vs observe)

- **touch→mouse** (`shepherd-touch-bridge`) **grabs**: it `EVIOCGRAB`s every
  touchscreen (`dev.grab()`), so the activity never sees raw touch. Output is a
  `/dev/uinput` absolute pointer.
- **gamepad** (`shepherd-gamepad-bridge`) **observes**: gilrs reads
  `/dev/input/event*` without `EVIOCGRAB`, so the activity still sees the raw
  gamepad alongside the synthesized mouse+keyboard. Intentional (targets
  activities that ignore gamepads).

### This environment's input stack

`/proc/bus/input/devices` shows **no real touchscreen and no relative-only
mouse used as the pointer**. The pointer devices are *absolute*:

- `event2` — "QEMU QEMU USB Tablet": `EV=1f` (ABS_X/Y + REL + buttons),
  emulated USB HW.
- `event4` — "spice vdagent tablet": `Bus=0000` virtual uinput device created
  by spice-vdagent; ABS_X/Y + buttons, also exposes `js0`.
- `event3` — PS/2 mouse (relative; normally idle).

udev tags all three `ID_INPUT_MOUSE`. Implications baked into the
implementation:

- Input is **already absolute**, so the hard "relative mouse → synthetic
  cursor tracking" problem doesn't apply; we read `ABS_X/ABS_Y` directly (the
  touch bridge's `DeviceRange` logic, run forward). This is why we scoped the
  deliverable to **tablet→touch** (absolute-in) rather than mouse→touch.
- Discovery can't key on `REL`. We select on `ABS_X && ABS_Y` + a contact
  button (`BTN_TOUCH`/`BTN_LEFT`/`BTN_TOOL_PEN`) and **exclude
  `INPUT_PROP_DIRECT`** — which skips real touchscreens *and our own synthetic
  touchscreen* (avoids a self-feedback loop).
- Multiple absolute tablets exist; auto-discovery grabs all matching ones.
- `spice-vdagent` may keep injecting pointer position; stop `spice-vdagentd`
  during a session if the host cursor still moves.
- Permissions: `/dev/uinput` and the event nodes are `root:input`; the dev user
  `shepherd-dev` is **not** in `input` (only `shepherd-kiosk` is). Run as the
  kiosk user or add the group to exercise the bridge.

## What was implemented

A new sidecar `shepherd-tablet-bridge` plus a shared touchscreen output sink.

### `shepherd-bridge` (shared output crate)

- `OutputEvent::{TouchDown, TouchMotion, TouchUp}` (slot + raw absolute coords
  with extents, matching the `PointerMotionAbsolute` convention).
- `UinputSink::new_touchscreen(output_scale)` — builds a `/dev/uinput`
  **touchscreen**: MT type-B axes (`ABS_MT_SLOT`, `ABS_MT_POSITION_X/Y`,
  `ABS_MT_TRACKING_ID`), single-touch `ABS_X/Y` mirror, `BTN_TOUCH`, and
  `INPUT_PROP_DIRECT` (so libinput classifies it as a touchscreen and the
  compositor delivers real `wl_touch`, not pointer events).
- Dispatch tracks active slots → tracking IDs, emits a fresh positive tracking
  ID per contact and `-1` on lift, and toggles `BTN_TOUCH` on the
  first-down / last-up transitions. Coordinates reuse `rescale_abs`
  (output-scale aware).

### `shepherd-tablet-bridge` (new crate)

Mirrors `shepherd-touch-bridge`'s structure (auto-discovery, per-device reader
threads, `EVIOCGRAB`, SIGTERM/SIGINT shutdown, `--device`/`--output-scale`).
Contact = `BTN_TOUCH || BTN_LEFT` (real pen tip *or* VM absolute-pointer
click), so click-drag → touch stroke and uncontacted motion (hover) emits
nothing. Single-finger (slot 0). `looks_like_tablet` excludes
`INPUT_PROP_DIRECT`.

### Wiring

- `InputCompatMode::TabletToTouch` (shepherd-api) and
  `RawInputCompat::TabletToTouch` (config schema), mapped in policy.
- Conflict guard: `touch_to_mouse` and `tablet_to_touch` invert each other; the
  later of the pair is dropped with a warning (prevents the tablet→synthetic
  touchscreen→mouse loop). Unit-tested.
- `spawn_tablet_bridge` + `tablet_bridge_binary` (`SHEPHERD_TABLET_BRIDGE_BIN`
  override) in host-linux `sidecar.rs`; adapter spawns it (reusing
  `touch_output_scale()`).
- Build/install plumbing: workspace member, `scripts/lib/build.sh` binary list,
  `install.sh` group comment, `dist/udev/71-shepherd-uinput.rules`,
  `config.example.toml`, and the crate README.

## Verification

`cargo build --workspace`, `cargo clippy --all-targets -D warnings`,
`cargo test` (bridge/tablet-bridge/config/api), `cargo fmt --all --check`, and
`validate-config config.example.toml` all pass. Not yet exercised end-to-end
against a live activity (needs the `input` group / kiosk user); the MT output
should be confirmed with `libinput debug-events` / `wev` showing touch (not
pointer) events.
