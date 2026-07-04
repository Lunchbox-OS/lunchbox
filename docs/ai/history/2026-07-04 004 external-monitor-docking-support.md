# External monitor / docking support — scope (issue #87)

Source: <https://git.armeafamily.com/albert/shepherd-launcher/issues/87>

## Issue text (verbatim)

> This probably only requires a modification to the Sway configuration.
>
> When using a gaming handheld or laptop, it may be convenient to plug in
> an external monitor or TV. This needs to not break the one activity at a
> time requirement.
>
> On first boot, we should detect which display is the primary.
>
> When a secondary display is connected, by default we should mirror the
> primary display. The HUD should also have an icon toggle that will disable
> the primary display and use the secondary's native resolution.

## Requirements, restated

1. Detect the primary display on first boot.
2. When a secondary display connects, **mirror** the primary onto it by default.
3. HUD gets an **icon toggle**: switch from mirror mode to "external only" —
   disable the primary output and drive the secondary at its native resolution.
4. Never break the **one-activity-at-a-time** invariant.

## Key finding: the issue's core assumption is wrong

> "This probably only requires a modification to the Sway configuration."

**Sway/wlroots cannot mirror outputs from config.** There is no `output`
directive that clones another output. The only supported way to mirror is an
external screencopy client — the de-facto tool is
[`wl-mirror`](https://github.com/Ferdi265/wl-mirror), which captures one output
via the `wlr-screencopy`/`ext-image-copy-capture` protocol and paints it
fullscreen onto another surface. See upstream
[sway#1666](https://github.com/swaywm/sway/issues/1666) and
[sway#5713](https://github.com/swaywm/sway/issues/5713).

Placing two outputs at the same logical coordinates does **not** mirror — sway
renders each output's own slice of the shared coordinate space, so a window
sized to one panel is cropped/black on the other. Mirroring is therefore *not*
a pure sway.conf change; it needs a runtime component.

This reframes the whole issue from "config tweak" to "small feature with a
compositor-driven state machine and a new HUD control."

## Why the one-activity invariant is at risk

Today the kiosk relies on a single logical output:
- `workspace 1 output *` binds the single workspace to all outputs.
- `for_window [app_id="shepherd-launcher"] fullscreen enable` fullscreens the
  launcher on whatever output it maps to.
- No keybindings exist to move focus/windows between outputs;
  `focus_follows_mouse no`.

When a second output connects, sway assigns it its **own** workspace. That
second workspace is an independent surface where a window could live —
breaking "one activity at a time" if anything ever maps there. **Mirroring
sidesteps this**: both physical panels show the same single logical output, so
there is still exactly one workspace and one activity. This is precisely why
the issue wants mirror-by-default rather than extend-by-default. The "external
only" toggle also preserves the invariant (one output active, one workspace).
The mode we must never fall into is *extend* (two active, independent outputs).

## Relevant existing machinery (this is mostly already built)

The plumbing to drive sway outputs from shepherdd already exists for the
XWayland HiDPI workaround (issue #45) and is a close template:

- `crates/shepherd-host-linux/src/sway.rs`
  - `get_outputs()` → `Vec<OutputScale { name, scale }>` (parses
    `swaymsg -t get_outputs --raw`, filters to `active`).
  - `set_output_scale(name, scale)`.
  - Both shell out to `swaymsg` and check the JSON `{success,error}` reply.
  - **Gap:** `RawOutput` currently only decodes `name/scale/active`. We'd want
    `make`/`model`/`serial`, `current_mode`, `focused`, and rect/position to
    identify the primary and drive modes. Easy extension.
- `crates/shepherdd/src/hidpi.rs` — `XwaylandHidpi` is the exact pattern to
  copy: a controller struct holding `Arc<IpcServer>` + `broadcast::Sender<Event>`,
  captures output state under a `Mutex`, mutates outputs, broadcasts an event so
  the HUD reacts. A new `DisplayController` would mirror this shape.
- `crates/shepherd-host-api/src/traits.rs` — `HidpiController` trait +
  `NoOpHidpiController`. A new `DisplayController` trait would sit here so HTTP-
  only/test contexts get a no-op.
- Host adapter already has a monitor task: `adapter.rs:363 start_monitor()`
  (`main.rs:187`). Detecting hotplug likely hangs off a similar loop or a sway
  IPC output-event subscription.

## HUD toggle wiring (also a well-trodden path)

- HUD is GTK4 layer-shell; `crates/shepherd-hud/src/{app,state,volume,brightness}.rs`.
- The **mute toggle** in `volume.rs` and brightness in `brightness.rs` are the
  model: an icon button whose click sends an IPC command to shepherdd, with the
  HUD updating on the broadcast event (`VolumeChanged`, `BrightnessChanged`).
- `state.rs` already subscribes to the shepherdd event stream. A new
  `DisplayModeChanged { mode }` event slots into `EventPayload`
  (`crates/shepherd-api/src/events.rs`) alongside `HudScaleChanged`.
- The toggle should only appear when a secondary display is present — HUD needs
  to know current display state (via the new event + an initial query).

## Command / API surface

Follows the `ManagementService` trait pattern
(`crates/shepherd-management/src/service.rs`), which auto-generates the RPC
surface via `#[management_rpc]`. Likely additions:
- `get_display_state() -> DisplayState` (outputs present, current mode).
- `set_display_mode(mode)` where `mode ∈ { Mirror, ExternalOnly }` (and
  implicitly `SingleInternal` when no secondary is attached).
- New `EventPayload::DisplayModeChanged` / `DisplaysChanged`.
- New `DisplayController` trait in `shepherd-host-api` + real impl in shepherdd
  + `NoOp` for tests/HTTP.

This also gives admins mirror/external control over HTTP/BLE for free, which is
consistent with volume/brightness.

## Open questions for the issue author

1. **Mirror implementation:** add a `wl-mirror` runtime dependency (extra
   surface + it draws its own window we'd have to keep on the secondary output,
   below the HUD), or is "extend but keep everything constrained to one
   workspace" acceptable? wl-mirror is the only way to get *true* mirroring;
   everything else compromises the invariant or the UX. Recommend wl-mirror.
2. **Resolution mismatch:** handheld panel (e.g. 1280×800) mirrored to a 4K TV —
   mirror at source resolution and let the TV upscale? "External only" mode
   dodges this by using the TV's native mode, which is the point of the toggle.
3. **"Detect primary on first boot":** what's the primary-selection rule —
   always the internal/`eDP-*`/`LVDS` panel, else first-enumerated? Persist the
   choice, or recompute each boot? Suggest: internal panel by connector-name
   heuristic, recomputed each boot (no persistence needed).
4. **Toggle persistence:** should "external only" survive unplug/replug or a
   reboot, or reset to mirror-by-default each time a secondary connects?
   Suggest: reset to mirror on each new connection (matches "by default we
   should mirror").
5. **Audio:** out of scope? Plugging in an HDMI TV usually implies wanting audio
   on it. Not mentioned in the issue; flagging.
6. **HUD placement in external-only mode:** HUD must render on the now-primary
   (external) output; confirm layer-shell surface follows the active output.

## Rough component checklist

- [ ] `shepherd-host-linux/src/sway.rs`: extend `RawOutput`; add
      `get_displays()`, `enable_output()/disable_output()`,
      `set_output_mode()`, primary-detection helper.
- [ ] `shepherd-host-api/src/traits.rs`: `DisplayController` trait + `NoOp`.
- [ ] `shepherdd/src/display.rs` (new): controller impl (mirror via wl-mirror
      subprocess mgmt / external-only via output enable/disable + mode), hotplug
      watcher, broadcasts events. Modeled on `hidpi.rs`.
- [ ] Wire hotplug detection (sway output events or poll in `start_monitor`).
- [ ] `shepherd-api/src/events.rs`: `DisplayModeChanged` (+ maybe
      `DisplaysChanged`).
- [ ] `shepherd-management/src/service.rs`: `get_display_state`,
      `set_display_mode` RPCs (auto-exposed to HTTP/BLE).
- [ ] `shepherd-hud`: icon toggle button, shown only when secondary present,
      subscribing to the new event.
- [ ] `sway.conf`: ensure second output doesn't spawn a usable second
      workspace (mode logic keeps one logical output active); confirm the
      launcher-fullscreen and no-second-workspace behavior.
- [ ] Tests: unit tests for output parsing/primary detection (pattern already
      in `sway.rs` tests); `shepherd-e2e` runs a real headless sway — check
      whether it can present a virtual second output (`WLR_HEADLESS_OUTPUTS`)
      to exercise hotplug.
- [ ] Docs: crate READMEs + `docs/INSTALL.md` if `wl-mirror` becomes a dep.

## Effort estimate

Medium. The output-driving and HUD-toggle patterns already exist and are
directly reusable, so the mechanical surface is small and low-risk. The real
cost/risk is concentrated in: (a) the mirror implementation choice
(`wl-mirror` integration vs. accepting a compromise) and (b) reliable hotplug
detection and primary selection across real hardware. Recommend resolving the
open questions — especially Q1 — before implementation, since the answer
determines whether this is a config+subprocess feature or something larger.
