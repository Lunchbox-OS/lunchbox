# shepherd-tablet-bridge

Per-activity sidecar that grabs absolute pointer / tablet devices and
re-emits their input as synthetic **touch** events, for activities that only
handle touch and ignore mouse or pen input. It is the inverse of
[`shepherd-touch-bridge`](../shepherd-touch-bridge/README.md).

## What it does

- Auto-detects every absolute pointing device under `/dev/input` (or takes
  explicit `--device` paths) — a graphics tablet / digitizer, or the absolute
  "tablet" a VM exposes (QEMU and SPICE present the guest pointer this way).
- `EVIOCGRAB`s each one so no other Wayland client receives the raw
  mouse/pen events while the bridge is running.
- Creates a `/dev/uinput` virtual **touchscreen** (the multitouch type-B
  protocol with `INPUT_PROP_DIRECT`) and translates motion into `wl_touch`
  contacts. Because the device is `DIRECT`, libinput classifies it as a
  touchscreen and the compositor delivers real touch events — not pointer
  events — to the activity. uinput is consumed by every compositor through
  libinput (wlroots, Mutter, KWin, X11), unlike the wlroots-only virtual
  protocols the sidecars used before (issue #58).
- Exits on `SIGTERM`/`SIGINT`; the kernel releases the grabs when the file
  descriptors close.

## Contact model

A touch contact is held while a **tip or button is down** — `BTN_TOUCH` (a
real tablet's pen tip) or `BTN_LEFT` (a VM absolute pointer's click). So:

- click-drag (or pen-tip-down + move) becomes a single touch stroke, and
- plain motion with nothing pressed — a hovering pen, or a moved-but-unclicked
  cursor — emits **nothing**, because touch has no hover.

Single-finger only (one contact, slot 0); the targeted sources report one
point at a time.

## Device selection

Devices that report `INPUT_PROP_DIRECT` are **skipped**: real touchscreens (and
this bridge's own synthetic touchscreen) already deliver touch, and grabbing
its own output would feed back on itself. A device qualifies when it has
absolute `ABS_X`/`ABS_Y` axes plus a contact button (`BTN_TOUCH`, `BTN_LEFT`,
or `BTN_TOOL_PEN`) and is not `DIRECT`.

Note that this also means **`tablet_to_touch` and `touch_to_mouse` are
mutually exclusive** — stacking them would form a loop (tablet → synthetic
touchscreen → mouse). Config validation drops the conflicting pair with a
warning.

## Flags

- `--device PATH` — grab a specific device; repeatable. Omit to auto-detect.

## Requirements

The user running the bridge must be in the `input` group (to read the source
evdev nodes and write `/dev/uinput`); see [docs/INSTALL.md](../../docs/INSTALL.md).

## Running it inside a VM (QEMU/SPICE)

A guest typically exposes the pointer as an **absolute** device (e.g. "QEMU
QEMU USB Tablet" and "spice vdagent tablet"), which this bridge grabs by
default. There may be more than one such device; all matching ones are grabbed
together. `spice-vdagent` may keep injecting pointer position — stop
`spice-vdagentd` during a session if the host cursor still moves.
