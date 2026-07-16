# Android on a fractional-scale panel: render at native scale 1 + density zoom (#2)

Branch `u/albert/2/android-activity`.

## Prompt

> try it with DPI set to 1.5 … the app appear 1.5x too large on launch … [later]
> that 61 shot is exactly the bug state I was referring to before with the window
> being too small - it's real on hardware … [target] 1.5× bigger UI, crisp …
> compensate [the HUD], but note that this is literally the same scaling performed
> by the XWayland hack … implement.

## The bug (one root cause, two faces)

On `output * scale 1.5`, Waydroid renders wrong: either the content is ~1.5×
magnified and clipped ("too large") or the window letterboxes ("too small on
hardware"). Both are the *same* defect: **Waydroid can't handle a fractional
`wl_output` scale.** It assumes the output's logical size equals its mode, and it
allocates its Wayland surface **buffer at session-boot time** for `mode × scale`.
`persist.waydroid.width/height` is ignored at fractional scale, and a mid-session
output-scale change is ignored too (the buffer is boot-locked). Proven by booting a
session at 1.5 (wm 1920×1080), dropping the output to 1 + forcing `wm size 1280` —
the host still magnified it because the *buffer* stayed 1920 (shots dpi-shots/90,91).

## The fix

Waydroid's **integer**-scale path is flawless (scale-1 baseline was pixel-perfect).
So: run the Waydroid session at native **scale 1** and express the panel's zoom as
Android **density** instead of output scale.

This is exactly what the XWayland HiDPI hack already does for Steam/XWayland games,
so we reuse it:

- **`service.rs`**: Android entries now set `needs_hidpi`, so at launch
  `hidpi.apply()` drops every output to scale 1, broadcasts `HudScaleChanged` (the
  HUD counter-scales so it isn't shrunk), and `dm.reassert()`s the docking mirror;
  `restore()` on session end. `HidpiController::apply()` now **returns** the captured
  scale, threaded into `SpawnOptions.android_ui_scale`.
- **`spawn_android`** (adapter): the output is scale 1 by then, but the *warm*
  session may have prebooted at 1.5 (wrong buffer). If `android_ui_scale > 1` and the
  `waydroid_scale1_booted` flag is unset, restart the session once — it now reboots
  at scale 1 with a correct buffer — then set density = base × scale via the new
  helper action **`scale-density <permille>`** (reads `Physical density`, sets the
  override to `physical × permille/1000`; 1500 = 1.5×). The flag is reset in
  `preboot`/`recover`/`repin` (all reboot at the grid's scale), so the first scaled
  launch after any of those pays a one-time session restart and the rest skip it.

## Docking interaction (why this composes)

`DisplayManager` only ever sets output **mode**, never **scale** (scale is owned by
`hidpi`), so forcing scale 1 can't fight the arrangement. Mirroring is `wl-mirror`
screencopy of the **primary**, so Waydroid only renders on the primary and the
external is just a copy — a no-op for this fix. External-only / dock transitions
change the primary's *mode*, which already triggers a `repin` session restart; that
restart resets the scale-1 flag, so the next launch re-verifies. Net new cost is
concentrated in extra Waydroid reboots on display changes, each mirrored.

## Validated end-to-end (headless)

Session booted at scale 1.5 (wm 1920×1080). Launch `android-khan` through shepherd:
log shows `Applied XWayland HiDPI workaround` + `Restarting Waydroid at native scale
1`; output → scale 1.0; wm size → 1280×720; `Override density: 270`; window rect
1280×720 (fills); crisp, HUD correct size (shot dpi-shots/A0). Overturns the earlier
"pin resolution to physical mode" approach (`bcbb18a`/`6943183`), which is a no-op at
fractional scale.

## Harness note

The dev VM's Android WindowManager wedges after several sequential boots ("wm size:
Broken pipe", windows stop mapping). Full `systemctl restart waydroid-container` +
fresh boot clears it; gate on a non-empty `wm size` before testing. `waydroid shell
screencap` shows Android's own framebuffer regardless of host GPU.
