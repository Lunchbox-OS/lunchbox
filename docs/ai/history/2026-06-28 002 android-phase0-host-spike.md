# 2026-06-28 — Android activity kind: Phase 0 host spike (results)

Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/2>
Scope: [2026-06-28 001 android-activity-kind-scoping.md](2026-06-28%20001%20android-activity-kind-scoping.md)

## Prompt

> commit the doc and start phase 0.

Phase 0 from the scoping doc: stand up Waydroid on the target, confirm the
launcher-integration mechanics (multi-window `app_id`, Sway fullscreen,
`am force-stop`, exit detection), and measure boot time + idle RAM — to
de-risk every "(verify on hardware)" item before writing any launcher code.

**Result: Phase 0 passed.** Every mechanic the runtime design depends on is
confirmed working. Details and corrections to the scoping assumptions below.

## Environment (differs from the scoping doc's assumption)

The scoping doc and CLAUDE.md assume Ubuntu **25.10**. The actual dev box is:

- **Ubuntu 26.04 "resolute"** (Resolute Raccoon), kernel **7.0.0-27-generic**.
- **A VM**: GPU is `Red Hat Virtio 1.0 GPU` (QEMU/KVM virtual). CPU is AMD
  x86_64. ~11.4 GB RAM. Active compositor in the dev session is **GNOME
  Shell**, not sway (sway is installed at `/usr/bin/sway`).
- Implication: Waydroid runs nested (LXC in a VM) with software/virtual GPU.
  Fine for validating launcher mechanics; **not** representative of GPU app
  performance (Minecraft Bedrock etc.). Real perf must be measured on kiosk
  hardware. Recall Waydroid does not support Nvidia GPUs.

## What was installed (retained for Phase 1+)

- Added the official repo for `resolute` (`curl https://repo.waydro.id | sudo
  bash -s resolute`) — **`resolute` is in the installer's supported codename
  list and has built packages**, so no `plucky` fallback was needed.
- `waydroid` **1.6.2** + the LXC stack (`lxc`, `liblxc-common`, `libgbinder`,
  …). Pulls in `waydroid-container.service`.
- **Kernel module**: `binder_linux` loads cleanly (`CONFIG_ANDROID_BINDER_IPC=m`,
  `CONFIG_ANDROID_BINDERFS=m`) — **no DKMS/anbox-modules needed**. Persisted via
  `/etc/modules-load.d/waydroid.conf` (`binder_linux`). The module is named
  `binder_linux`, not `binder` (`modprobe binder` fails).
- `waydroid init` (VANILLA) downloaded **LineageOS 20 / Android 13, x86_64**:
  `system.img` 1.81 GB, `vendor.img` 561 MB in `/var/lib/waydroid/images/`.
  Chose **VANILLA over GAPPS** deliberately: the integration mechanics are
  identical with/without Google Play, and vanilla skips the manual Play
  certification dance. GAPPS is a Phase-4 concern for the real wishlist apps.

After the spike the session + container service were stopped; the package and
images remain installed for the next phases.

## Mechanics confirmed

Tested by running a **headless nested sway** (`WLR_BACKENDS=headless`, output
`HEADLESS-1` 1280x800) with the production matching rule
`for_window [app_id="^waydroid\..*"] fullscreen enable`, starting a Waydroid
session pointed at that sway, and launching built-in apps
(`com.android.calculator2`, `org.lineageos.jelly`).

| Mechanic | Result |
|----------|--------|
| Multi-window mode | `waydroid prop set persist.waydroid.multi_windows true` (reads back `true`); takes effect after a **session restart**. |
| Per-app Wayland toplevel `app_id` | **`waydroid.<package>`** confirmed exactly — e.g. `waydroid.com.android.calculator2`. Window title = human app name (`Calculator`). Matches the `wayland-hwc.cpp` prediction. |
| Sway fullscreen rule | The `for_window [app_id="^waydroid\..*"]` rule **fired**: window came up `fullscreen_mode=1`, rect = full output (1280×800). |
| App launch | `waydroid app launch <pkg>` works **as the session user**; toplevel appears ~4 s after the command. |
| Force-stop / `stop()` | `waydroid shell am force-stop <pkg>` → the toplevel **disappears within ~1 s**. |
| Exit detection | The force-stop (and by the same mechanism, a user closing the app) destroys the toplevel, **observable from the compositor immediately** → window-destroy is a viable `HostEvent::Exited` trigger (design B1). |
| Liveness / foreground queries | `... shell pidof <pkg>` returns the in-container PID; `... shell dumpsys activity activities | grep ResumedActivity` returns the foreground activity (`org.lineageos.jelly/.MainActivity`). Useful for authoritative state / design B2. |

## Important corrections & gotchas for the implementation

1. **`waydroid shell` requires root.** `waydroid app launch`, `waydroid prop
   get/set`, and `waydroid status` run as the **session user**, but `waydroid
   shell` (→ `am force-stop`, `dumpsys`, `pidof`) **needs root** (otherwise:
   `RuntimeError: Action "shell" needs root access`). The host adapter's
   `stop()` / liveness path must invoke these via root. `shepherdd` already
   runs privileged, so this is fine — but the split matters: launch on the
   user bus, control via root.
2. **`waydroid shell` stdout** is suppressed unless you pass the **global**
   flag `--details-to-stdout` *before* the action:
   `sudo waydroid --details-to-stdout shell pidof <pkg>`. Without it you get a
   "Use '--details-to-stdout' …" hint and no output. (The command still runs;
   only stdout capture is affected.) Any parsing of `pidof`/`dumpsys` output
   must use this flag.
3. **Readiness signal**: poll the **session log** for `Android with user 0 is
   ready`, *not* `getprop sys.boot_completed` over `waydroid shell` (that path
   needs root and doesn't return on stdout cleanly). The preboot task should
   watch the session-start log line.
4. **Multi-window needs a session restart** to take effect after setting the
   prop — set it once at provisioning / before first preboot, not per launch.
5. Outputs keep their boot resolution; on a fixed-resolution kiosk this is
   fine, but the Android surface won't rescale to arbitrary Sway resizes.

## Measurements (this VM — re-measure on real hardware)

- **Warm session start → "ready": ~10 s** (images extracted, container warm).
  First-ever cold boot after `init` was longer (first-boot dexopt etc.).
  Strongly supports the **preboot + keep-warm** strategy in the scoping doc.
- **Idle RAM for a booted vanilla session: ~0.8 GB** (host `used` rose
  1520 → 2307 MB on boot, returned to ~1534 MB after session stop). The
  scoping doc's unverified "few hundred MB–~1 GB" now has a measured point.
- CPU after boot is near-idle. `persist.waydroid.suspend` / `container
  freeze` (to drive RAM/CPU lower while warm) were **not** separately
  benchmarked — a Phase-1 task.

## Net effect on the plan

Phase 0 confirms the runtime design is sound and nothing is a blocker:

- The vertical-slice path is: **launch as user → match `waydroid.<pkg>`
  toplevel (already supported by `sway::list_windows`) → fullscreen via
  `for_window` → detect exit via toplevel-destroy → on stop, root
  `am force-stop`.** Recommend going straight to **design B1 (window-watch)**
  rather than the B2 pid-proxy — the toplevel-destroy signal proved clean and
  immediate, so the pid-proxy shim isn't needed.
- Update the scoping doc's "(verify on hardware)" items: app_id string ✓,
  `resolute` repo ✓, binder built-as-module/no-DKMS ✓, warm boot ~10 s ✓,
  idle RAM ~0.8 GB ✓. Still open: real-hardware GPU perf, GAPPS app sign-in,
  suspend/freeze RAM savings.
- New must-dos captured for Phase 2: root for `waydroid shell`,
  `--details-to-stdout` for parsing, log-based readiness, multi-window set at
  provisioning.

## Next: Phase 1

Land `EntryKind::Android { package_name, .. }` + the mechanical match sites and
the `[service.waydroid]` schema (preboot / multi_window / suspend_when_idle /
boot_ready_timeout), per the file-by-file map in the scoping doc. No spawn
wiring yet — pure config/plumbing behind the capability flag.
