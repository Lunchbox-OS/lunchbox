# Brightness slider in the HUD

User prompt (Claude Opus 4.7):

> Implement screen brightness control via a slider for devices that support
> it, much like the volume controls

## Approach

Mirror the existing volume pipeline end-to-end so the two indicators are
operationally identical and shepherdd remains the single point that enforces
restrictions.

Layers touched:

- `shepherd-host-api/src/brightness.rs` — new `BrightnessController` trait
  alongside `VolumeController`, plus `BrightnessStatus`,
  `BrightnessCapabilities`, `BrightnessRestrictions`, and `BrightnessError`.
  There is intentionally no mute analog: a panel at 0% reads as "screen
  off", a useless state on a kiosk where the launcher is the only way back.
- `shepherd-host-linux/src/brightness.rs` — `LinuxBrightnessController`.
  Scans `/sys/class/backlight/*` for a device, reads `max_brightness` and
  the current `brightness` straight from sysfs (world-readable), and writes
  through `brightnessctl` (added to `scripts/deps/run.pkgs`). The original
  draft also had a sysfs-write fallback, but it was removed in a follow-up
  because that path only works as root: on a normal install shepherdd runs
  as the desktop user and only `brightnessctl`'s shipped udev rules grant
  the `video` group write access. Falling back silently would hide a real
  misconfig in production, so we now log a warning at startup if the binary
  is missing and report `available=false` instead.
- `shepherd-api` — new `BrightnessInfo` / `BrightnessRestrictions` types,
  `Command::{GetBrightness, SetBrightness}`,
  `ResponsePayload::{Brightness, BrightnessSet, BrightnessDenied}`, and
  `EventPayload::BrightnessChanged`. Same shape as the volume protocol so
  clients can copy/paste their handling.
- `shepherd-config` — `RawBrightnessConfig` / `BrightnessPolicy` with
  `max_brightness`, `min_brightness`, `allow_change`. Global + per-entry
  override, same lookup order as `volume`.
- `shepherdd` — constructs a `LinuxBrightnessController`, threads it through
  the IPC dispatcher, exposes the same restrictions lookup helper as volume,
  and broadcasts `BrightnessChanged` so every subscribed client (HUD,
  launcher, HTTP SSE) sees changes immediately.
- `shepherd-http` — new `/api/v1/brightness` (GET, PUT) handler under
  `handlers/brightness.rs`. Returns `available=false` instead of 500 when
  no backlight is present so UIs can hide the slider cleanly. New
  `MockBrightness` in `tests/api.rs` to keep the integration tests
  closed-box.
- `shepherd-hud` — `crate::brightness` IPC helper, a new
  `BrightnessInfo` watch channel in `SharedState`, and a new
  `.brightness-control` slider in the HUD bar between the volume slider
  and the network indicator. The box is hidden entirely when
  `info.available == false`, so the HUD on a desktop machine looks
  unchanged. The slider reuses the volume slider's debounce + drag-tracking
  scaffolding (50 ms quiet window, "user is dragging" flag so events don't
  fight the cursor).

## Notes / decisions

- `brightnessctl` is a hard runtime dep (in `scripts/deps/run.pkgs`).
  Reads still go through sysfs because `brightness`/`max_brightness` are
  world-readable and a `Command::output()` per HUD tick would be
  needlessly heavy.
- Brightness restrictions intentionally don't have a "muted" notion. The
  policy knobs are `max_brightness`, `min_brightness`, `allow_change`.
  Setting `min_brightness = 10` is the kid-friendly default suggested in
  the example config so the screen never goes fully dark.
- The HUD's brightness icon palette is the standard freedesktop
  `display-brightness-{low,medium,high}-symbolic`. The slider's `highlight`
  fill uses the existing `--color-warning` (warm/amber) to visually
  distinguish it from the volume slider's `--color-info` (cool/blue).
- Per-tick HUD update is the same pattern as volume: read cached state, push
  it to the widget unless the user is actively dragging. No polling on the
  brightness controller itself — `BrightnessChanged` events drive the cache.

## Verification

- `cargo build --workspace` and `cargo test --all-targets` pass.
- `cargo clippy --all-targets -- -D warnings` clean.
- `validate-config config.example.toml` succeeds.
- This dev machine has no backlight, so the HUD slider stays hidden as
  expected (`available=false` from the controller). Live UI verification on
  a laptop is still TODO.

## Follow-up: hardware brightness keys

Added support for `XF86MonBrightnessUp` / `XF86MonBrightnessDown` by
mirroring the volume-key plumbing:

- `Command::BrightnessUp { step }` and `Command::BrightnessDown { step }`
  in `shepherd-api`. Same shape as `Volume{Up,Down}` so they reuse the
  one-shot CLI helper.
- `Service::handle_relative_brightness` in `shepherdd`: reads current
  level, clamps to `BrightnessRestrictions`, calls `set_brightness`, and
  broadcasts `BrightnessChanged`. Lifted from `handle_relative_volume`
  almost verbatim.
- `--brightness-up` / `--brightness-down` flags on `shepherd-launcher`
  (default step 5%). The existing `send_volume_command` helper got
  generalized to `send_media_command` so it accepts either
  `Volume*`/`Brightness*` responses.
- `sway.conf` binds `XF86MonBrightness{Up,Down}` to those flags with
  `--locked` so they still work on the lock screen.

No new HTTP routes — the keypress path is IPC-only, matching how the
volume keys work.
