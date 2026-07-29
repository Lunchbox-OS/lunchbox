# Issue 114: HUD elements are not sized correctly when the XWayland DPI hack is enabled

<https://git.armeafamily.com/albert/shepherd-launcher/issues/114>

## Prompt

> fix #114
>
> [follow-up] the repro needs both a non-1x DPI set and the XWayland DPI hack
> enabled on an activity

Issue body:

> When the XWayland DPI hack is enabled, the following components are the wrong
> size:
>
> - Warning text
> - Slider controls -- making them hard to control with the touchscreen

## Background

The XWayland DPI hack is the issue #45 workaround (`xwayland_native_resolution`
on an entry): while such an activity runs, shepherdd drops every sway output to
`scale 1.0` so the XWayland client renders on the panel's native pixel grid, and
broadcasts `HudScaleChanged { factor }` with the captured pre-launch scale. The
HUD is layer-shell and lives in logical pixels, so it counter-scales by that
factor to keep its physical size — see `2026-05-17 001
xwayland-hidpi-scale-toggle.md`.

`apply_scale` implements the counter-scale by multiplying every `Npx` literal in
the HUD's own stylesheet (`scale_px_literals`), plus the icon `set_pixel_size`
values and the two slider `width_request`s in the timer.

## Root cause

The counter-scale only reaches sizes the HUD's *own* stylesheet states. Any
dimension left to the GTK theme keeps its logical-pixel value, so when sway drops
to scale 1.0 it renders 1/factor too small on screen. Two such dimensions were
visible:

1. **Warning text.** `.warning-text` set a colour and weight but no
   `font-size`, so the banner label rendered at the GTK default font (here
   `Adwaita Sans 11` ≈ 14.7px) — unscaled. Every other HUD label happens to name
   its own `font-size`, which is why only this one looked wrong. The confirm-close
   popover's Cancel / End button labels had the same gap (its *message* label
   sets 15px, the buttons inherited the theme default).

2. **Slider knob.** `.volume-slider slider` / `.brightness-slider slider` set
   `min-width`/`min-height: 12px`, but the theme's own slider node is **16px**,
   so the theme won the size at factor 1.0 and the counter-scaled 12px only
   reached 18px at factor 1.5 — *smaller* than the 24px it should have been, i.e.
   a touch target that shrinks exactly when the panel is HiDPI. The theme's
   `-8px` slider margin has the same problem: unscaled, it also let the trough
   thicken once the knob grew.

Measured on the headless 1920x1080 output (physical px, warning banner up):

| config | bar | slider knob | warning text cap | trough |
| --- | --- | --- | --- | --- |
| scale 1.0, factor 1.0 | 54 | 16 | 9 | 4 |
| scale 1.5, factor 1.0 (**what the hack should match**) | 81 | 23 | 16 | 6 |
| scale 1.0, factor 1.5 (hack, before) | 80 | **18** | **10** | 6 |
| scale 1.0, factor 1.5 (hack, after) | 80 | **24** | **16** | 6 |

The bar height, icons, text with an explicit size, and the trough were already
correct — this was specifically the theme-supplied sizes.

## Changes

All in `crates/shepherd-hud/src/app.rs` (`CSS_TEMPLATE`):

- `.hud-bar` gains `font-size: 14px`, and `.confirm-close-popover > contents`
  the same. Setting the base size on the two root nodes means a label that does
  not name its own size inherits a *scaled* one instead of falling back to the
  theme default — this covers `.warning-text`, the popover buttons, and any
  label added later.
- `.volume-slider slider` / `.brightness-slider slider`: `min-width`/
  `min-height` 12px → 16px, plus an explicit `margin: -8px`. Both values are
  what the theme picks on its own, so at factor 1.0 nothing changes; stating
  them is what lets them scale.
- New `app::tests` module: `scale_px_literals` behaviour, plus two regression
  tests asserting the text roots declare a scalable `font-size` and the slider
  knob is stated at ≥ the theme's 16px.

Not changed: the `gtk4::Box` spacings, which are widget properties rather than
CSS and remain unscaled (the deliberate compromise recorded in the #45 note).
They are why the post-fix HUD is still slightly more tightly packed under the
hack; every element's own size now matches.

## Verification

Headless dev session (`headless-dev` skill), config fixture with two otherwise
identical entries differing only in `xwayland_native_resolution`, so the same
HUD state can be shot with the hack off and on:

```sh
./scripts/shepherd dev headless --size 1920x1080 --config <fixture>
# swaymsg output HEADLESS-1 scale 1.5   (via scripts/lib/headless.sh headless_run)
printf '{"request_id":1,"api_version":1,"method":"launch","params":{"id":"<id>"}}\n' \
    | nc -U dev-runtime/shepherd.sock
./scripts/shepherd dev shot out.png
```

Gotchas worth knowing for the next agent doing this:

- **`stop_current` restores the output scale asynchronously.** The RPC returns
  before shepherdd's HiDPI restore lands, so a `swaymsg output … scale 1.0`
  issued right after a hacked session ends gets clobbered a second later. Wait
  (~8s) and re-read `swaymsg -t get_outputs` before trusting the scale, and
  assert the scale again at screenshot time.
- **Verify the HUD state in the pixels, not by elapsed time.** A launch that
  races shepherdd's startup (or hits an already-busy daemon) leaves a
  "No session" bar that still screenshots fine; the volume slider then renders
  in its disabled state, which measures differently. Poll for the warning
  banner's background colour before keeping a shot.
- `cargo test --all-targets` needs several GB of free disk for the e2e test
  binaries; `target/debug/incremental` is a good thing to delete first.
