# Make the input sidecars usable outside of shepherd-launcher (#58)

Forgejo issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/58>

## The request (verbatim from the issue)

> The input sidecars (`shepherd-touch-bridge`, `shepherd-gamepad-bridge`;
> triggered by `input_compat` in the config) are so useful on a gaming
> handheld that I genuinely miss them when it is running a regular desktop
> session.
>
> Unfortunately, they are currently written to require wlroots compositors,
> of which Ubuntu's default GNOME is not:
>
> ```
> $ shepherd-gamepad-bridge --preset productivity
> ...
> Error: compositor does not support zwlr_virtual_pointer_v1 (a wlroots-only
> protocol); the bridge requires a wlroots compositor such as Sway. If you're
> running this standalone, launch it from inside the shepherd-launcher Sway
> session (or `./run-dev`), not from your desktop's Wayland session
> (KWin/Mutter/etc.)
> ```
>
> The sidecars should be rewritten to work on any Wayland, not just wlroots.

## Why they only work on wlroots today

Both bridges emit synthetic input through wlroots-specific Wayland protocols:

- `zwlr_virtual_pointer_v1` — pointer motion/buttons/scroll. wlroots-only.
  - `shepherd-touch-bridge/src/main.rs` (`manager.create_virtual_pointer`)
  - `shepherd-gamepad-bridge/src/wl.rs` (`pointer_manager`)
- `zwp_virtual_keyboard_v1` — key events. wlroots + a few others, **not**
  Mutter/GNOME. `shepherd-gamepad-bridge/src/wl.rs`.

GNOME (Mutter) and KWin do not advertise the virtual-pointer global, so
`WaylandOutputs::connect()` / the touch bridge's registry roundtrip fail with
the error above.

This was a deliberate original trade-off. The touch-to-mouse design doc
(`docs/ai/history/2026-05-09 002 touch-to-mouse.md`) lists as a feature:

> **No `/dev/uinput`.** The Wayland virtual-pointer protocol means we don't
> need root or a uinput udev rule.

#58 reverses that trade-off: portability across compositors is now worth a
uinput permission grant.

## Key observation: the payload is already evdev

The hard part is already done — both bridges speak evdev natively on *both*
ends:

- **Input:** touch bridge grabs `/dev/input/event*` via `evdev` +
  `EVIOCGRAB`; gamepad bridge reads gamepads via `gilrs` (evdev underneath).
- **Output:** the events they synthesize are *already raw evdev codes*.
  `shepherd-gamepad-bridge/src/preset.rs` defines `BTN_LEFT = 0x110`,
  `KEY_W = 17`, etc., with the comment: "the Wayland virtual-keyboard
  protocol takes raw evdev codes." The touch bridge emits `BTN_LEFT` and
  `motion_absolute`.

So the synthetic-input payload is already in exactly the form a `/dev/uinput`
virtual device wants. The wlroots protocol is only the last hop.

The gamepad bridge also already has a clean output seam: a pure `OutputEvent`
enum + a `WaylandOutputs` sink, with all preset/mapping logic Wayland-free and
unit-tested (`preset.rs`). Swapping the backend is well isolated.

## Approaches considered

### A. `/dev/uinput` kernel virtual device — chosen

Create a virtual mouse + keyboard via uinput. The kernel exposes it as a real
input device that *every* compositor consumes through libinput — wlroots,
Mutter, KWin, even X11. Compositor-agnostic by construction (this is how
`ydotool` works).

- **Fit:** near-ideal. Both bridges already produce evdev codes, so the
  mapping is ~1:1, and the xkb-keymap-upload dance in `wl.rs` disappears
  entirely — the compositor applies the *user's own* layout to raw keycodes.
- **Costs:**
  - Needs write access to `/dev/uinput` — one udev rule, mirroring the
    existing `input`-group setup (`docs/INSTALL.md`).
  - A freshly created uinput device needs a brief settle before the
    compositor binds it, or the first events drop.
  - Absolute positioning for the touch bridge (it uses `motion_absolute`) is
    the one fiddly part: create an `ABS_X`/`ABS_Y` pointing device and let
    libinput map it to the output. Needs real-hardware verification.

### B. libei + `org.freedesktop.portal.RemoteDesktop`

The sanctioned modern path GNOME/KWin actually want; `reis` crate exists.
Rejected for now: the portal normally shows a permission dialog and needs a
session — awkward for an unattended kiosk sidecar — and is heavier to
implement. Good future direction (Phase 2+), poor near-term fit.

### C. Hybrid: `OutputSink` trait with wlroots + uinput backends

Keep wlroots as a fast path, fall back to uinput. Rejected as the default
because uinput is a strict superset of where wlroots works (it works in Sway
too), so two code paths earn their keep only for deployments that refuse a
`/dev/uinput` rule. The `OutputSink` abstraction is introduced anyway, so
re-adding a wlroots backend later is cheap if needed.

## Decision

Switch both bridges to a **uinput backend behind a shared `OutputSink`
abstraction, and drop the wlroots protocols** (Approach A). One tested output
path; deletes the keymap hack; works everywhere the wlroots path did and more.

## Implementation plan

### Phase 1 (this change)

1. **New crate `shepherd-bridge`** (library) — the shared output seam:
   - `OutputEvent` enum (moved out of `preset.rs`), extended with
     `PointerMotionAbsolute { x, y, x_extent, y_extent }` for touch.
   - `ScrollAxis`, plus `keycode` / `btncode` constants (moved from
     `preset.rs`).
   - `trait OutputSink { dispatch / frame / flush / destroy }`.
   - `UinputSink` implementing it: a relative pointer + keyboard device for
     the gamepad bridge, and an absolute pointer device for the touch bridge.
     Includes a post-create settle so the compositor binds the node before we
     emit.
2. **Migrate `shepherd-gamepad-bridge`:** depend on `shepherd-bridge`, import
   `OutputEvent`/preset constants from it, replace `WaylandOutputs` with
   `UinputSink`, delete `wl.rs` and the `wayland-*` dependencies.
3. **Migrate `shepherd-touch-bridge`:** replace the inline Wayland client with
   `UinputSink` (absolute), drop `wayland-*` dependencies. `emit_update`
   becomes a thin translation to `OutputEvent::PointerMotionAbsolute` +
   `PointerButton`.
4. **Permissions/setup:** add a udev rule granting the kiosk user
   `/dev/uinput` access; wire it into the `scripts/shepherd install` flow
   alongside the existing group setup; update `docs/INSTALL.md` and
   `CONTRIBUTING.md`. This is the one real operational change for users.
5. **fmt / clippy / tests / config validation.**

The pure preset logic (`preset.rs`) and the touch normalization math are
unchanged, so the existing unit tests remain the strong test surface.

### Watch-outs

- **Absolute touch positioning** is the highest-risk detail — prototype on
  real hardware. uinput abs device declares a fixed range up front; the touch
  bridge rescales its per-device `(value, extent)` into that range.
- **uinput device settle** — add a readiness wait / initial sync after build.
- **Keyboard layout** — uinput keycodes go through the compositor's *active*
  layout. The gamepad WASD mapping is positional, so this is actually more
  correct than the old forced-US keymap, but worth a sanity check.
- **Grab loop** — ensure the touch bridge's touchscreen auto-discovery does
  not re-grab the new uinput device.
- **e2e tests** run headless Sway (`docs/ai/history/2026-05-02 001 e2e
  tests.md`); a uinput-backed bridge is testable there (Sway reads uinput via
  libinput) but CI containers must expose `/dev/uinput`.

### Phase 2 (optional, later)

- Re-add a feature-gated wlroots backend if any deployment can't grant
  `/dev/uinput`.
- Add a libei/portal backend for sandboxed / portal-first environments.

## Backend mapping reference (evdev 0.13)

- Build: `VirtualDevice::builder()?.name(..).with_keys(..)?` +
  `.with_relative_axes(..)?` (gamepad) or `.with_absolute_axis(..)?` (touch),
  then `.build()?`.
- Emit: `VirtualDevice::emit(&[InputEvent])`, where
  `InputEvent::new(EventType::KEY.0 | RELATIVE.0 | ABSOLUTE.0, code, value)`,
  terminated by a `SYN_REPORT` (`EventType::SYNCHRONIZATION.0, 0, 0`) per
  frame.
- Keys/buttons → `EV_KEY` (value 1/0). Relative motion → `REL_X`/`REL_Y`.
  Scroll → `REL_WHEEL`/`REL_HWHEEL` (sign-flipped vs. the Wayland convention
  the preset layer uses). Absolute → `ABS_X`/`ABS_Y` rescaled to the device's
  declared range.
