# 2026-07-12 — DPC + GAPPS device-owner: prototyped on the dev box (works)

## Prompt

Follow-up to the source+apt install/admin reconcile ([2026-07-11 001]). The user
asked what it would take to get the DPC app installed, we walked the theoretical
DPC + GAPPS provisioning sequence (flagging that neither the GAPPS runbook nor
the DPC+GAPPS combination had ever been run), then: "prototype Phase 1-2 against
the local Waydroid (yes, you may wipe it)."

Phase 1 = fresh GAPPS instance. Phase 2 = install the DPC and set it as device
owner. This is the empirical result, correcting the guesses in [2026-06-28 008]
Part B and [2026-06-28 009].

## Headline

**`dpm set-device-owner` SUCCEEDS on a Waydroid GAPPS image** — even with the
device already "provisioned" (`device_provisioned=1`, `user_setup_complete=1`,
which GAPPS auto-completes). Final state:
`admin=com.armeafamily.shepherd.dpc/.AdminReceiver, DeviceOwner, Affiliated,
isOrganizationOwnedDevice=true`. The theoretical "already provisioned / Setup
Wizard" gate is a **non-issue** here. **The only gate is `accounts=0`** — so the
one hard ordering rule holds: set device owner *before* any Google sign-in.

## Environment

Dev box: Ubuntu 26.04 "resolute" VM, Waydroid 1.6.2, software-rendered (QEMU
virtio GPU), x86_64. SDK build-tools 35.0.0 + platform android-35 present.
Sessions were brought up on a nested headless sway, same pattern as
`scripts/integration-tests/test-waydroid.sh`.

## Phase 1 — fresh GAPPS instance ✅

```sh
sudo waydroid session stop; sudo systemctl stop waydroid-container
sudo waydroid init -f -s GAPPS      # VANILLA -> GAPPS; ~90s; system.img 1.81 -> 2.58 GB
```
- Switched `system_ota` to the GAPPS channel; vendor stayed MAINLINE (only the
  system image is re-downloaded).
- **GOTCHA:** `init -f` re-initialises **configs + images only**. It does **NOT**
  wipe `~/.local/share/waydroid/data` — a stale package registration and
  `device_policies.xml` from the June DPC work survived the "wipe." A genuine
  clean slate needs a separate `sudo rm -rf ~/.local/share/waydroid/data` (the
  container recreates a fresh /data on next boot). The first probe pass tested
  the wrong thing because of this.

## Phase 2 — install the DPC and set device owner ✅ (two GAPPS-only snags first)

Building the APK is unchanged (`ANDROID_SDK_ROOT=/opt/android-sdk
./dpc-waydroid/build.sh` → `com.armeafamily.shepherd.dpc`, signed). Installing it
on GAPPS surfaced two things vanilla never showed:

1. **`waydroid app install <apk>` silently no-ops** in the headless/scripted
   nested-sway context (exit 0, nothing installed) — even with
   `DBUS_SESSION_BUS_ADDRESS` set. The reliable method is to push the APK into
   the real `/data/local/tmp` and `pm install` over root shell:
   ```sh
   sudo install -D -m 0644 dpc-waydroid/shepherd-dpc.apk \
       ~/.local/share/waydroid/data/local/tmp/dpc.apk        # == container /data/local/tmp
   sudo waydroid --details-to-stdout shell -- pm install -r -g /data/local/tmp/dpc.apk
   ```
2. **Play Protect (Finsky VerifyApps) delays package registration by ~15 s on
   GAPPS.** `pm install` prints `Success`, but `pm list packages` does not show
   the package for up to ~15 s (logcat: `VerifyApps: Timeout for second result:
   14998ms`). A naive `install; sleep 2; check` reports "missing." Vanilla has no
   GMS, so installs register instantly — which is exactly why the *original*
   vanilla DPC verification never hit this. Also: Android `pm` output is CRLF —
   strip `\r` (`tr -d '\r'`) before an exact `grep`.

Once the DPC is actually registered:
```sh
sudo waydroid --details-to-stdout shell -- dpm set-device-owner \
    com.armeafamily.shepherd.dpc/.AdminReceiver
# -> "Success: Device owner set to package com.armeafamily.shepherd.dpc/.AdminReceiver"
```
Verified across two states (owner removed + re-attempted each time):

| accounts | device_provisioned | set-device-owner |
|---------:|-------------------:|:-----------------|
| 0        | 0                  | ✓ Success        |
| 0        | 1 (GAPPS default)  | ✓ Success        |

Owner removal (dev), confirmed working, matches the README: session down →
`rm ~/.local/share/waydroid/data/system/{device_owner_2.xml,device_policies.xml}`
→ restart. (Android 13 also keeps a 1-byte `device_policies_version`, left alone.)

## Still open (Phase 3 — needs a real Google account)

Not yet tested, and the genuinely uncertain part:
1. **Google sign-in AFTER device owner is set** — the DPC declares no
   `DISALLOW_MODIFY_ACCOUNTS`, so in principle an account can be added post-owner,
   but this is unverified on Waydroid.
2. **Play behaviour on a managed, uncertified GAPPS instance** — whether a
   personal (or paid, e.g. Bedrock) account works when the device is a
   fully-managed device owner. This is the highest-risk unknown.
3. **Device certification** — register the GSF Android ID at
   google.com/android/uncertified before Play works.

These are interactive / account-bound and can't be driven headlessly.

## Practical takeaway

The DPC+GAPPS *device-owner* half is real and low-friction on a fresh instance:
GAPPS init → clean /data → install DPC via pm → set-device-owner (before sign-in).
Lock Task ⊥ multi-window ([2026-06-28 009]) is unchanged and still the true
integration blocker for wiring the DPC into shepherd. `lock_down` remains the
default lock-in.

Probe scripts retained in the session scratchpad (`phase2-*-v6/v7`,
`phase2-install-debug-v5`).
