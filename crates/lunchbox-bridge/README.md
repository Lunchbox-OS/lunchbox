# lunchbox-bridge

Shared output backend for the input-compat sidecars
(`lunchbox-touch-bridge`, `lunchbox-gamepad-bridge`).

## Why this exists

Both bridges read real input devices (touchscreens, gamepads) and synthesize
mouse + keyboard events for activities that ignore raw touch/gamepad input.
Originally they emitted those events through wlroots-only Wayland protocols
(`zwlr_virtual_pointer_v1`, `zwp_virtual_keyboard_v1`), which restricted them
to Sway and other wlroots compositors (see issue #58).

This crate replaces that last hop with a compositor-agnostic backend built on
the kernel's `/dev/uinput`. A uinput virtual device is consumed by *every*
compositor through libinput — wlroots, Mutter (GNOME), KWin, even X11 — so the
bridges now work in any Wayland session.

## What it provides

- [`OutputEvent`] — the backend-agnostic synthetic-input vocabulary
  (relative/absolute pointer motion, buttons, scroll, keys). Both bridges'
  pure mapping logic produces these.
- [`OutputSink`] — the trait the bridge main loops drive (`dispatch` per
  event, `frame` per logical batch, `flush`).
- [`UinputSink`] — the `OutputSink` implementation. `new_relative()` builds a
  relative pointer + keyboard device (gamepad bridge); `new_absolute()` builds
  an absolute pointer device (touch bridge).

The events carry raw Linux evdev codes (`BTN_LEFT`, `KEY_W`, …), which is
exactly what uinput wants — so no keymap translation is needed; the
compositor applies the user's own keyboard layout to the raw keycodes.

## Permissions

Emitting through uinput requires write access to `/dev/uinput`. The kiosk user
is granted this via a udev rule installed by `scripts/lunchbox install`; see
`docs/INSTALL.md`.
