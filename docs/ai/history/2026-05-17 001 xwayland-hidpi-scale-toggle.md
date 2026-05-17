# Issue 45: XWayland activities are blurry on HiDPI displays

<https://git.armeafamily.com/albert/shepherd-launcher/issues/45>

## Background

Issue #7 was closed by shipping `sway.conf.d` drop-ins, so an admin can
set `output * scale 1.5` once and have it persist across reinstalls. That
fixes the launcher and Wayland-native activities, but XWayland clients
still see only the **logical** resolution: on a 1080p panel with scale
1.5, an XWayland game's screen is 1280x720 and sway upscales the buffer,
producing a soft image. Sway does not have a `xwayland scale` knob
(KDE/Hyprland do; this has been a known gap upstream).

## Approach

Per-activity opt-in flag that temporarily drops sway's compositor scale
to 1.0 for the duration of an activity, then restores it on exit. The
HUD (layer-shell, Wayland-native) would normally appear physically
smaller while the scale is dropped, so shepherdd emits an
`EventPayload::HudScaleChanged { factor: f64 }` event with the captured
pre-launch scale; the HUD multiplies its CSS px values and window height
by this factor so it stays at its usual physical size.

Two paths considered and rejected for the first pass:

- **gamescope wrapper.** Better for games specifically (GPU upscaling,
  FSR/integer scaling, frame caps) but requires an extra runtime
  dependency, and Steam-snap + gamescope interactions need verification on
  the actual kiosk. Documenting it as a `kind = "process"` wrapper is a
  follow-up if #1 leaves Steam games unhappy.
- **Documentation-only.** Works for `kind = "process"` (the user can
  prepend `gamescope -- …` themselves), but does not cover Steam, Snap,
  or Flatpak entries where shepherdd controls the argv.

## Mechanism

`xwayland_native_resolution: bool` on `RawEntry`/`Entry`. When `true`,
shepherdd:

1. On launch (after engine approves, before host.spawn): calls
   `shepherd_host_linux::get_outputs()` via `swaymsg -t get_outputs`,
   captures each active output's scale, and runs
   `swaymsg output <name> scale 1.0` for any output not already at 1.0.
   Broadcasts `HudScaleChanged { factor }` where `factor` is the maximum
   captured scale (max so multi-output kiosks err on the readable side).
2. On exit (host event SessionEnded, StopCurrent, or graceful shutdown):
   broadcasts `HudScaleChanged { factor: 1.0 }` first, then restores
   each output's saved scale.

State lives in `Arc<XwaylandHidpi>` threaded through the lifecycle
handlers (`handle_command`, `handle_host_event`, `handle_core_event`).
`restore` is idempotent so duplicate calls (e.g. StopCurrent followed by
the host's eventual Exited event) are harmless.

The HUD's `apply_scale` regenerates the stylesheet by scaling every
`Npx` literal by the factor (`scale_px_literals`) and resizes the
layer-shell surface (`set_default_height` + `set_exclusive_zone`). Icons
(`set_pixel_size`) and a few `width_request` hardcoded values are left
unchanged for simplicity — text and bar height dominate visual
perception, and the icons just shrink proportionally.

## Decisions

### Why a bool instead of `display_compat` enum

`input_compat` already enumerates orthogonal sidecars, so mirroring it
with `display_compat = "xwayland_native_scale"` was tempting. An enum
with one variant is over-engineered; a follow-up gamescope mode can
either be its own boolean or a new enum field added then. CLAUDE.md says
don't design for hypothetical future requirements.

### Why apply before spawn (not after)

The XWayland client queries screen dimensions on first map. If sway is
still at 1.5 when the window maps, the client thinks the screen is
1280x720 and may pick that as its render resolution permanently
(depending on the toolkit). Applying first means the client sees
1920x1080 from the start.

### Why broadcast `HudScaleChanged` after the scale change on apply, and before on restore

On apply, sway drops to 1.0 first (HUD briefly looks tiny), then the HUD
gets `factor=1.5` and grows back. On restore, HUD shrinks to 1.0 first
(briefly looks normal-sized against logical 1.0 sway), then sway
restores 1.5 (HUD appears correctly sized). The reverse orderings each
have a frame where the HUD is briefly oversized, which is more jarring.

### What the HUD does not scale

`gtk4::Image::set_pixel_size(20)`, `Scale::width_request(100)`, and a
few other hardcoded widget dimensions are not multiplied by factor.
Doing so requires holding refs to all the widgets in the timer closure
and updating each on change. Skipped because the primary HUD content
(text and bar height) does follow the factor, and the icons being one
size smaller at factor 1.5 is a cosmetic compromise. Easy to extend if
needed.

### HTTP launch path: shared via a trait

`shepherd-http/src/handlers/sessions.rs` has its own copy of the launch
and stop flows. To keep parity without making shepherd-http depend on
shepherd-host-linux, the workaround is exposed as a small trait
[`HidpiController`] in `shepherd-host-api` (next to `HostAdapter`):

- `XwaylandHidpi` in shepherdd implements it; it now owns the IPC and
  broadcast handles so the trait can be `apply(&self) / restore(&self)`
  with no extra arguments at the call site.
- `AppState` carries `Arc<dyn HidpiController>`.
- The HTTP launch handler calls `state.hidpi.apply()` before
  `host.spawn` for entries with `xwayland_native_resolution = true`, and
  `restore()` on the spawn-error path; the stop handler calls
  `restore()` before `host.stop()`. Both paths share the same instance
  with the IPC handlers, so `apply` is correctly rejected as already-active
  if both transports are racing.
- `NoOpHidpiController` ships alongside the trait for tests and any
  future host that lacks a sway compositor.

## Changes

- `shepherd-api/src/events.rs`: new `EventPayload::HudScaleChanged { factor: f64 }`.
- `shepherd-config/src/schema.rs` + `policy.rs`: new
  `xwayland_native_resolution: bool` on raw and validated entry.
- `shepherd-host-api/src/traits.rs`: new `HidpiController` trait and
  `NoOpHidpiController` impl alongside `HostAdapter`.
- `shepherd-host-linux/src/sway.rs`: `get_outputs()` and
  `set_output_scale(name, scale)` via `swaymsg`, plus unit test for the
  outputs JSON parser.
- `shepherd-host-linux/src/lib.rs`: re-export the new helpers.
- `shepherdd/src/hidpi.rs`: `XwaylandHidpi` implements `HidpiController`;
  owns the IPC server and event broadcast channel.
- `shepherdd/src/main.rs`: thread `Arc<XwaylandHidpi>` through the four
  lifecycle entry points (Launch handler, StopCurrent, handle_host_event,
  handle_core_event, graceful shutdown); cast to `Arc<dyn HidpiController>`
  for the HTTP `AppState`.
- `shepherdd/Cargo.toml`: pull in `async-trait`.
- `shepherd-http/src/state.rs`: `AppState.hidpi: Arc<dyn HidpiController>`.
- `shepherd-http/src/handlers/sessions.rs`: HTTP launch reads
  `xwayland_native_resolution` from the entry and calls
  `state.hidpi.apply()` pre-spawn / `restore()` on spawn error; stop
  handler calls `restore()` before `host.stop()`.
- `shepherd-hud/src/state.rs`: track scale factor; handle
  `HudScaleChanged` (with clamp 0.5..=4.0 against bad inputs).
- `shepherd-hud/src/app.rs`: split `load_css` into
  `install_css_provider` + `apply_scale`; rebuild stylesheet from a
  template with per-px scaling; resize the layer-shell surface on
  change; observe scale in the existing 500 ms timer.
- `shepherd-launcher-ui/src/state.rs`: ignore the new event variant
  (HUD-only).
- `config.example.toml`: commented-out example entry showing the flag.
- Test fixtures updated to add `xwayland_native_resolution: false`;
  `shepherd-http` API tests construct `AppState` with `NoOpHidpiController`.

No new runtime dependencies. No changes to `sway.conf` or the
`shepherd.conf.d` mechanism from issue #7 — that one remains the path
for the persistent base scale.
