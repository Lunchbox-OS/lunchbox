# Waydroid fractional-scale: reopen squash diagnosis (#2)

Branch `u/albert/2/android-activity`. Diagnosis-only session — no code change landed
(the per-open session restart from `2920dfe` remains the shipped stopgap). The
upstream-facing write-up lives in
[`docs/ai/waydroid-fractional-scale-upstream.md`](../waydroid-fractional-scale-upstream.md).

## Prompt

> I mean, yes, this works, but it's way too slow. Try to figure out the differences
> between leibniz and your headless environment here, then debug what's going on

Context: on a `output * scale 1.5` kiosk, Android activities rendered correctly only
via a Waydroid session reboot at output scale 1 on *every* launch (~30 s). Skipping
the reboot on reopen squashed the app into the top-left quarter of the screen.
Earlier headless attempts to reproduce the reopen squash had contradicted what
leibniz (real hardware) did.

## Why the headless environment diverged from leibniz

1. **Mode-set race.** `sway.conf` execs shepherdd at sway startup, but the headless
   harness applies `--size` via `swaymsg output … mode …` *after* sway is up. So
   shepherdd's Waydroid preboot could read the virtual output while it was still at
   its default 1280x720 — on leibniz the panel is 1920x1080 from the first frame.
   Faithful repro requires the mode *and* scale at config time, e.g.
   `/etc/sway/shepherd.conf.d/dpi.conf` (included by `sway.conf`):

   ```
   output HEADLESS-1 mode 1920x1080
   output * scale 1.5
   ```

   plus `--size 1920x1080` so the harness's own mode-set is a no-op.
2. **Runtime vs config-time scale.** Leibniz sets scale 1.5 in the sway config
   (before any client connects); earlier tests flipped it at runtime.
3. **State contamination.** Manual `waydroid prop set` / `wm` pokes from earlier
   experiments had left stale values (e.g. a bogus 1280x720 resolution pin).

With those fixed, the reopen squash reproduces deterministically through the real
shepherd flow (fast-path binary: first open correct, reopen half-size).

## Root cause

Waydroid's in-container hwcomposer speaks **`wp_fractional_scale_v1`** (binary
strings: `get_fractional_scale`, `preferred_scale`, `set_buffer_scale`) and
**latches its scale once, at session boot**. Evidence:

- `waydroid.display_scale` (a live Android prop the hwcomposer writes) **never
  changes on a warm session**, across repeated live output-scale flips (watched for
  ~10 s per flip).
- If the warm client processes *any* exposure to a scale other than its boot scale
  — a live flip while idle at the grid, **or** flips queued while the container was
  `lxc-freeze`-frozen and replayed on thaw — every **subsequent** surface presents
  at exactly **half size, top-left anchored** (960 px wide content on a 1920 px
  output; half linear = the user-reported "top-left quarter" by area). Consistent
  with a latched `wl_surface.set_buffer_scale(ceil(1.5) = 2)`; not confirmed at the
  protocol level.
- Android's own state stays **correct** throughout: `wm size`, density override,
  and task bounds (`am task resize` succeeds, `dumpsys` shows full-display bounds).
  The breakage is purely Wayland presentation.
- The latch is client-global and permanent; only a session reboot clears it.

## Fix candidates tested (all but reboot fail)

| candidate | result |
|---|---|
| skip the reboot on reopen (flag optimization, `c393d91`) | half-squash, deterministic |
| freeze the container across the flips, thaw after scale is back at 1 | identical half-squash — queued events replay on thaw |
| restart only the composer HAL (`kill` `composer@2.1-service`, ~2 s) | kills every Wayland surface (shepherd ends the session) and later `waydroid app launch` wedges |
| let Waydroid handle 1.5 natively (no pin, no scale drop) | broken differently: content as a 2/3-size block — this is the *original* "1.5x too small" bug |
| session reboot while output is at scale 1, zoom via `wm density` | pixel-perfect (the shipped behavior) |

## Other findings

- A "squashed to top-left ~1280x720" look **also** arises from an *unmaximized*
  freeform task (direct `waydroid app launch` without shepherd's `maximize`
  helper). Distinct failure from the half-size latch; easy to conflate.
- `waydroid prop set persist.*` silently no-ops when **no session is running**, so
  preboot's resolution pin never lands on a fresh container (leibniz's
  `persist.waydroid.width=1920` is a legacy value from an old running session).
  Latent bug if pinning ever matters again.
- Unpinned boot at scale 1.5 sizes the Android display to *usable logical area x
  scale* (e.g. (720-54 HUD exclusive zone) x 1.5 = 999 → `wm size 1920x999`) and
  sets `ro.sf.lcd_density = base x scale` on its own.
- Idle-suspend (`lxc` FREEZE) interacts badly with all of this operationally: a
  frozen container fails `waydroid session start` with "container failed to start"
  (`lxc-info` shows FROZEN; `lxc-unfreeze` repairs it), and frozen clients replay
  queued scale events on thaw.

## Conclusion / proposed direction

Correctness requires the Waydroid client **never observe an output scale other
than 1**. Fast reopens therefore mean eliminating scale flips entirely: run the
output at scale 1 permanently and produce the 1.5x UI per-surface — launcher grid
self-scales, HUD uses its existing `HudScaleChanged` factor statically, Android
gets `wm density` (base x scale, proven pixel-perfect), Chrome entries get
`--force-device-scale-factor`. Steam/XWayland already forces scale 1 during play,
so that flip is *removed*, not added. `dpi.conf`'s `output * scale 1.5` would be
replaced by a shepherd-level UI-scale setting. Proposed to the user; awaiting
design sign-off. The upstream hwcomposer fix (see the companion doc) would remove
the constraint at the source.
