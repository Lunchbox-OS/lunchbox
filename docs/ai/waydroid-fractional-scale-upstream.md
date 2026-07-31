# Waydroid: hwcomposer latches fractional output scale at boot — upstream fix brief

A self-contained brief for working on the **upstream Waydroid bug** that forces
shepherd-launcher to reboot the Waydroid session on every Android activity launch
on fractional-scale (HiDPI) kiosks. No shepherd knowledge is required to work on
this; shepherd-specific history is in
`docs/ai/history/2026-07-17 001 waydroid-fractional-scale-diagnosis.md`.

**Status: a patched hwcomposer is installable today**, as a tarball with
install/uninstall scripts, from
[issue #119](https://git.armeafamily.com/albert/shepherd-launcher/issues/119);
an upstream Waydroid PR is pending. Operators need it on any kiosk with a
non-integer `output * scale` — see the "Patch the hwcomposer" step in
[`docs/INSTALL.md`](../INSTALL.md#0-patch-the-hwcomposer-hidpi-panels-only).

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

## Patch validation status (2026-07-19)

A patched `hwcomposer.waydroid.so` (installed via the Waydroid overlay at
`/var/lib/waydroid/overlay/vendor/lib64/hw/`) was validated against the criteria
above on the headless rig (1920x1080 mode, wlroots/sway, pixman):

- **Warm live-flip relaunch: PASS.** Boot at scale 1 → flip 1.5 → flip 1 →
  relaunch renders identical to a fresh boot (previously: permanent half-size).
- **Freeze/thaw flip relaunch: PASS.** Same flips queued across
  `lxc-freeze`/`lxc-unfreeze`: relaunch correct.
- **Kiosk end-to-end: PASS.** Full launcher flow at config-time `output * scale
  1.5`: one scale-1 session restart on the first open, then four consecutive
  close/reopen cycles each **1-2 s** and pixel-perfect (was ~30-60 s per open).
- **Fractional steady state (booted at scale 1.5): improved but still wrong.**
  No squash and crisp 1:1 presentation, but the Android display is sized
  logical x scale^2 (1280x720 logical -> `wm size 2880x1620` instead of
  1920x1080) — the scale looks double-applied — so layouts overflow and clip on
  the right/bottom third. `waydroid.display_scale` correctly reports 1.5 at boot
  but still does not track warm output-scale changes (boot-latched; the fix
  evidently re-evaluates per surface, which is what the kiosk flow needs).

Remaining upstream work: the steady-state double-scale above, and dynamic
`preferred_scale` tracking on warm sessions.

### Second patch iteration (2026-07-20, hash 6f9aa8c5…)

Revalidated in full; all previous passes **hold** (warm live-flip relaunch,
freeze/thaw relaunch, kiosk end-to-end: one restart on first open then reopens at
1–2 s, pixel-perfect).

- **`waydroid.display_scale` now tracks live output-scale changes in both
  directions** within ~3 s on a warm session (1.0→1.5→1.0 observed) — the
  boot-latch on the *scale value* is fixed.
- **The Android display still does not reconfigure on a warm change**: after the
  tracked scale changes, `wm size` keeps its boot-time dimensions. Harmless for
  relaunch-based flows (new surfaces are correct) but a live foreground app does
  not adapt.
- **The steady-state double-scale is unchanged**: booting with the output at
  scale 1.5 still yields `wm size 2880x1620` (logical x scale^2) instead of
  1920x1080, content clipped past the visible third. Since the *tracking* fix
  lives in the hwcomposer and behaves correctly, the boot-time display sizing —
  possibly in a different component (e.g. the display HAL /
  `vendor.waydroid.display@…` service or wherever the initial display geometry is
  derived) — still multiplies by the scale twice. That is now the single
  remaining defect blocking pin-free fractional operation.

## Workaround shipped in shepherd meanwhile

Reboot the Waydroid session while the output is at scale 1 before every launch
that follows a scale exposure, and express the UI zoom as Android density
(`wm density = base x scale`). Pixel-perfect but costs a session boot (~10–40 s)
per activity open — which is why this upstream fix is worth doing.

**Update (2026-07-29): the per-open cost is gone, the constraint is not.**
shepherd now boots the *preboot* session with the outputs held at scale 1 and
restores the fractional scale afterwards, so the warm session is already on the
native pixel grid and no launch has to reboot it. Measured on the faithful rig:
first open **81.2 s -> 6.9 s**, reopen 5.9 s -> 2.2 s, and the Android display
finally sizes to the panel (`wm size` 1920x999 -> 1920x1080). See
[`2026-07-29 003`](history/2026-07-29%20003%20android-first-open-native-scale-preboot.md).

This does not reduce the need for the upstream fix — it just moves the one
unavoidable boot to startup. The steady-state double-scale above is still what
blocks running Waydroid at a fractional scale at all, and shepherd still pays a
window at boot where the whole UI sits at scale 1 while Android comes up. A
hwcomposer that derived its geometry from the physical mode, and reconfigured on
warm scale changes, would let shepherd drop the scale dance entirely.
