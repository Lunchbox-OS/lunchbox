# Issue 2: making the *first* Android open fast

<https://git.armeafamily.com/albert/shepherd-launcher/issues/2>

## Prompt

> what is the lifetime of an Android activity relative to when it is requested
> and the app's activity according to Android itself? I'm trying to track down
> the source of the delay between when any Android activity is opened for the
> first time and when it actually appears, during which the launcher is spinning
> "Loading" while the HUD doesn't show that any activity is running (which also
> makes it impossible to cancel the load)
>
> [then] broadcast SessionStarted before the spawn, and SessionEnded on spawn failure
>
> [then] do the clean, then attack the scale-1 restart so the first open is fast

## Background

Follows the fractional-scale work in `2026-07-16 002`, `2026-07-17 001`, and
[`waydroid-fractional-scale-upstream.md`](../waydroid-fractional-scale-upstream.md).
The patched hwcomposer made *reopens* fast (1–2 s) but left one scale-1 session
restart on the first open, because preboot booted the session at the launcher
grid's fractional scale.

## Two separate defects behind one symptom

**1. The HUD could not see a slow launch.** `ManagementService::launch` held its
`SessionStarted` broadcast until after `host.spawn` returned. The engine had the
session from `start_session` the whole time — that call *returns* a
`CoreEvent::SessionStarted` and the caller dropped it on the floor. So for the
whole spawn the launcher span a spinner, the HUD drew nothing, and there was no
stop affordance. Fixed by broadcasting at `start_session` time; every early
return then owes a retraction, so `abort_announced_session` emits
`SessionEnded` + `StateChanged` on both the spawn-failure and entry-not-found
paths.

**2. The first open paid a full session reboot.** Preboot cleared
`waydroid_scale1_booted`, so the first fractional-scale launch stopped the warm
session and booted it again at scale 1.

## Measurements (faithful rig)

`output * scale 1.5` + `output HEADLESS-1 mode 1920x1080` both at *config* time
(`/etc/sway/shepherd.conf.d/`), `--size 1920x1080`, patched hwcomposer overlay
installed, container cold before each run:

| | before | after |
|---|---|---|
| first open | **81.2 s** | **6.9 s** |
| reopen | 5.9 s | 2.2 s |
| `wm size` | 1920x999 | 1920x1080 |

Screenshot-verified: full-screen, crisp, correct landscape layout, HUD on top.

## The fix

Preboot drops every output to scale 1 for the duration of the session boot and
restores afterwards, then sets `waydroid_scale1_booted`. Waydroid latches its
display geometry from the output scale observed at *session boot* and cannot be
corrected warm, so that is the only moment the scale matters. Net: **one** boot
at startup instead of two.

The flag is only set when preboot actually watched the session boot inside that
window — adopting an already-running session tells us nothing about its boot
scale, so that case still re-verifies the slow way. The `spawn_android` restart
remains as the fallback for wedge recovery and the docking repin.

Cost: a startup window (~64 s cold here) where the launcher/HUD render at scale
1 and look physically smaller while Android boots. A boot-time cosmetic in place
of a mid-use stall.

## Readiness desync (found while measuring)

`recover_wedged_waydroid` and `repin_waydroid_resolution` emit
`KindReadinessChanged { Android, false }` directly, but the readiness watcher
kept a *local* `last` cache. After an out-of-band re-gate the watcher still read
`true`, short-circuited every later poll, and never re-emitted — **Android
stayed hidden from the launcher until shepherdd restarted**. On this rig the
repin fires at every startup, so it reproduced immediately. The cache is now
shared (`waydroid_ready_last`) and both paths record what they published.

## Environment notes (cost real time)

- **The polkit *rules* file was not installed** on this machine — only the
  action policy. Without `/etc/polkit-1/rules.d/50-shepherd-waydroid.rules`,
  every `pkexec shepherd-waydroid-helper` call fails, so `boot_completed()`
  returns false forever and Android never un-gates. Installed from
  `dist/polkit/`. Worth checking on any box where Android tiles never appear.
- **Waydroid needs a session D-Bus.** The headless rig's `--user` mode gives a
  private `XDG_RUNTIME_DIR` with no bus, so `waydroid session start` dies with
  "Unable to autolaunch a dbus-daemon without a $DISPLAY". Run the rig as a user
  with a real login session instead.
- **`waydroid status` is not per-user.** A session owned by another user reads
  as RUNNING, so preboot adopts it and then `waydroid app launch` fails (exit 1)
  because it cannot reach that user's session bus. Only bites on a multi-user
  dev box, not a kiosk.
- The resolution pin still no-ops on a cold container (`waydroid prop set
  persist.*` needs a running session) — the latent bug noted in `2026-07-17 001`
  is still latent. The native-scale boot makes it not matter for geometry.
