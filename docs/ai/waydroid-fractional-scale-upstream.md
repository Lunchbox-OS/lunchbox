# Waydroid: hwcomposer latches fractional output scale at boot — upstream fix brief

A self-contained brief for working on the **upstream Waydroid bug** that forces
shepherd-launcher to reboot the Waydroid session on every Android activity launch
on fractional-scale (HiDPI) kiosks. No shepherd knowledge is required to work on
this; shepherd-specific history is in
`docs/ai/history/2026-07-17 001 waydroid-fractional-scale-diagnosis.md`.

## TL;DR

Waydroid's in-container hwcomposer reads the compositor's (fractional) output
scale **once, at session boot**, and never again. If the wl_output scale changes
while the session is warm — even while the LXC container is frozen, since queued
events replay on thaw — the presentation layer latches a wrong buffer scale
**permanently**: every surface created afterwards displays at exactly **half its
intended size** (top-left anchored) on a scale-1 output. Android-side state
(display size, density, task bounds) remains correct; only the Wayland
presentation is wrong. The sole recovery is a full `waydroid session stop/start`.

## Environment where this was characterized

- Waydroid **1.6.2** (`waydroid.tools_version=1.6.2`), LineageOS 20 / Android 13
  x86_64 GAPPS image (`system_ota=.../lineage/waydroid_x86_64/GAPPS.json`),
  vendor MAINLINE.
- Compositor: sway on wlroots (Ubuntu 26.04 "resolute"), both a real 1920x1080
  LVDS panel and a wlroots headless output (pixman *and* GLES against virtio-gpu).
  Behavior is identical on real hardware and headless.
- `persist.waydroid.multi_windows=true` (each app is its own toplevel,
  `app_id="waydroid.<package>"`).

## Minimal reproduction (no shepherd required)

Prereqs: a sway session (headless works: `WLR_BACKENDS=headless sway`), a Waydroid
session with multi-window on, any installed app (`org.khanacademy.android` used
below; the stock calculator works too).

```sh
OUT=HEADLESS-1            # or the real output name
APP=org.khanacademy.android

# 1. Boot the session with the output at scale 1. Renders correctly.
swaymsg "output $OUT scale 1"
waydroid session start &  # wait for boot: waydroid shell getprop sys.boot_completed == 1
waydroid app launch $APP  # window appears; content correct (see "maximize" note below)

# 2. Close the app, flip the output scale away and back (simulates a kiosk
#    showing its scale-1.5 launcher between Android activities).
waydroid shell am force-stop $APP
swaymsg "output $OUT scale 1.5"; sleep 5
swaymsg "output $OUT scale 1";   sleep 2

# 3. Relaunch. BUG: every new surface now displays at HALF size, top-left.
waydroid app launch $APP
```

Recovery: only `waydroid session stop` + `session start`.

Variants, all confirmed:

- The flip poisons even if the container is **frozen** across it
  (`lxc-freeze -n waydroid -P /var/lib/waydroid/lxc` before the flips,
  `lxc-unfreeze` after the scale is back at 1): the wl events queue in the socket
  and replay on thaw with the same result. There is no event-ordering workaround.
- Booting the session **while the output is at scale 1.5** is broken differently:
  the display sizes itself to (usable logical area x 1.5) and
  `ro.sf.lcd_density` is multiplied by the scale, but the content presents as a
  ~2/3-size block instead of filling the output (fractional presentation is wrong
  even in the steady state it chose itself).
- Distinguish from an unrelated cosmetic: launching without maximizing leaves a
  small *freeform task* in the corner. Rule it out by resizing the task to the
  display (`am task resize <taskId> 0 0 <w> <h>`; task id from
  `dumpsys activity activities` → `mCurTaskIdForUser={0=<id>}`). In the bug
  state, the task bounds are full-display and `wm size` is correct, yet the
  surface still displays at half size — that is the signature.

## Evidence gathered

- **`waydroid.display_scale` never updates warm.** The hwcomposer writes this
  Android prop (`waydroid shell getprop waydroid.display_scale`). It reflects
  the output scale at session boot (e.g. `1.500000`) and stays fixed through any
  number of live output-scale changes, observed over multi-second windows with
  surfaces mapped and unmapped. Scale handling is boot-once.
- **Binary strings** in
  `/var/lib/waydroid/rootfs/vendor/lib64/hw/hwcomposer.waydroid.so`:
  `get_fractional_scale`, `preferred_scale`, `set_buffer_scale`, plus the config
  props `persist.waydroid.width/height`, `persist.waydroid.width_padding/…`,
  `persist.waydroid.use_subsurface`, `persist.waydroid.no_background_subsurface`.
  So it binds `wp_fractional_scale_v1` and uses integer
  `wl_surface.set_buffer_scale` — presumably render-at-`ceil(scale)` with a
  viewport downscale, which matches the failure arithmetic below.
- **The failure is exactly x1/2.** After the poisoning flip (boot scale 1,
  exposure to 1.5), a 1920 px-wide render occupies exactly 960 physical px.
  `ceil(1.5) = 2` latched as buffer scale on a scale-1 output halves everything.
  (Inferred; not confirmed with a protocol dump — a `WAYLAND_DEBUG=1` capture of
  the hwcomposer's connection would confirm.)
- **Android internals stay right.** In the poisoned state: `wm size` correct,
  density override intact, `am task resize` succeeds and `dumpsys` shows
  full-display task bounds. Only presentation is wrong, for **new and existing**
  surfaces alike, client-wide, until session reboot.
- **Composer restart is not a workaround.** Killing the in-container
  `android.hardware.graphics.composer@2.1-service` (SurfaceFlinger respawns it)
  destroys every Wayland surface and subsequent `waydroid app launch` calls
  frequently wedge the platform bridge. Not viable.

## Where to look (to be confirmed against source)

The hwcomposer lives in the `android_hardware_waydroid` repository (the
`hwcomposer/` Wayland backend; the binary above is its build). Based on the
strings and behavior, the suspect areas are:

1. Where the `wp_fractional_scale_v1.preferred_scale` event is handled: it
   appears to be consumed only during initial surface/display setup. A warm
   session ignores subsequent events (neither `waydroid.display_scale`, the
   Android display config, nor the buffer scale of newly created surfaces track
   the compositor's current value — yet *something* latches from a flip, since
   post-flip surfaces present at half size while pre-flip ones were correct).
2. The interaction between the cached per-display scale and **new surface
   creation**: new surfaces after a flip get a `set_buffer_scale`/viewport
   combination inconsistent with the buffers Android renders.
3. The scale-1.5 *steady state* (booted at 1.5) rendering a 2/3-size block —
   the viewport destination appears to be set in logical units but presented as
   though physical (or vice versa).

## What a fix should do

1. Handle `preferred_scale` dynamically: on change, update the cached scale,
   resize/reconfigure the Android display (as a boot currently does), update
   `waydroid.display_scale`, and re-commit surfaces with consistent
   buffer-scale/viewport state.
2. Failing that, a much smaller fix that still unblocks kiosks: **re-evaluate the
   scale at every surface creation** instead of using the boot-time cache, so a
   stopped-and-relaunched app picks up the current scale. (Kiosks can guarantee
   no app surface exists across scale changes; they cannot guarantee the client
   never sees one.)
3. Make the fractional steady state actually correct (buffer = physical size,
   viewport destination = logical size), so compositors with fractional scale
   work without any pinning at all.

## Validation criteria

On a scale-1 output with a warm session: flip the output to 1.5 and back (both
live and across an lxc freeze/thaw), relaunch an app → renders identically to a
freshly booted session. Then the same with the session *booted* at 1.5 → content
fills the output crisply (buffer at physical resolution, not logical, not
magnified). `waydroid.display_scale` tracks the compositor's current value.

## Workaround shipped in shepherd meanwhile

Reboot the Waydroid session while the output is at scale 1 before every launch
that follows a scale exposure, and express the UI zoom as Android density
(`wm density = base x scale`). Pixel-perfect but costs a session boot (~10–40 s)
per activity open — which is why this upstream fix is worth doing.
