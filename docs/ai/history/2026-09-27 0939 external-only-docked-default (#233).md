# External-only as the default docked mode (#233)

Prompt: "implement #233, then push a PR for it"

## Issue text (verbatim)

> **Make external-only the default docked mode**
>
> Then clicking the display output button in the HUD will switch into the
> mirrored mode instead.
>
> The default should be configurable.

## What changed

- `[service.display] docked_mode = "external_only" | "mirror"` (default
  `external_only`). `DisplayManager` enters it wherever it used to pick
  `Mirror`: when a new external display connects, and in the "same display,
  but we were somehow in `SingleInternal`" case. A replug still resets to the
  configured mode rather than whatever the toggle last left, as before (issue
  #87's decision #4).
- The HUD needed no change: its button calls `DisplayMode::toggled()`, which
  already flips `ExternalOnly` ↔ `Mirror`, and it labels itself from the
  broadcast state.
- The config editor's "External displays" section has a picker for it.
- Two commits: the setting (default still `mirror`, no behaviour change), then
  the default flip, so the second one is the whole behaviour change.

## Verified headlessly

`lunchbox dev headless`, `swaymsg create_output` to dock, `output HEADLESS-N
unplug` to undock, `set_display_mode` on `dev-runtime/lunchbox.sock`:

- Docking disables HEADLESS-1, runs no `wl-mirror`, and broadcasts
  `ExternalOnly`.
- Toggling goes to `Mirror` (both outputs on, `wl-mirror HEADLESS-1` running).
- Undock → redock comes back as `ExternalOnly`.
- 58 dock/(toggle)/undock cycles at 2s gaps without a problem.

## Found along the way: sway crashes on undock after the screen blanks

Not caused by this change, but made the common path by it. In the headless
session (sway 1.11, wlroots 0.19.2, libwayland-server 1.24.0):

1. dock, in external-only (the new default — or the old default after
   toggling to external-only; both reproduce);
2. leave it idle until swayidle blanks the screen (`lunchbox-launcher
   --screen-off` → "Screen power set on=false", ~120s);
3. unplug the external display.

sway segfaults as soon as the output goes away (`segfault … in
libwayland-server.so.0.24.0`, and once a general protection fault at the same
offset), taking the whole session with it. A variant — blank, toggle to mirror,
undock, redock into external-only — crashes ~1.5s after the redock. The same
sequences without the idle blank did not crash in 58 cycles, and neither did
the old default when the blank happened in mirror mode (blank, then toggle to
external-only, undock, redock into mirror).

What this does *not* establish: whether real DRM hardware does the same (the
headless backend's outputs are unlike a real connector — HEADLESS-1 is
destroyed, not just disabled, and names are never reused), and whether it has
anything to do with #232 (there the HUD died, not sway). The likely shape is
sway or wlroots acting on an output that was powered off while the only other
output was disabled; the next step is a sway debug build and a backtrace, and
trying the same sequence on the device with a real TV.
