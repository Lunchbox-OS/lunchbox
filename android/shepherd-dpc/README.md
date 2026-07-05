# shepherd-dpc

A minimal Android **Device Policy Controller** for Waydroid kiosk lock-in. Set
as **device owner** (no Google account needed on a fresh Waydroid), it pins a
kiosk app in **Lock Task Mode** — blocking HOME, Recents, the status bar, and
launching other apps at the framework level. This is the gold-standard
containment, stronger than the statusbar-flag `lock_down` shepherd applies by
default.

> **Status: built and verified, but NOT wired into shepherd by default.** Lock
> Task Mode is *incompatible* with shepherd's current Android windowing — see
> [Important limitation](#important-limitation-lock-task--multi-window) below.
> Treat this as a verified building block for a future full-UI integration.

## Build

No Gradle — a plain SDK pipeline (`aapt2 → javac → d8 → zipalign → apksigner`):

```sh
ANDROID_SDK_ROOT=/opt/android-sdk ./build.sh    # -> shepherd-dpc.apk
```

The signing keystore (`dpc.keystore`, gitignored) is generated on first build
and **kept** — once the app is device owner it can only be updated with the same
key. Build artifacts and the APK are gitignored.

## Components

- `AdminReceiver` — the `DeviceAdminReceiver`; its existence is what lets the app
  be device owner.
- `LaunchActivity` — no-UI entry point. Allowlists a target package for Lock
  Task, then launches it with `ActivityOptions.setLockTaskEnabled(true)` so even
  apps that never call `startLockTask()` (Calculator, M365, …) get pinned.
- `ControlReceiver` — clears the Lock Task allowlist so a session can be ended.
- Holds `QUERY_ALL_PACKAGES` (a DPC must resolve launch intents for arbitrary
  kiosk packages; Android 11+ package visibility otherwise hides them).

## Provisioning + use (verified on Waydroid)

```sh
# install the DPC
waydroid app install shepherd-dpc.apk
# set it as device owner (fresh Waydroid, no accounts) — needs root
sudo waydroid shell -- dpm set-device-owner com.armeafamily.shepherd.dpc/.AdminReceiver
# launch a kiosk app pinned in Lock Task Mode (note the `--` so waydroid forwards
# the dashed args to am)
sudo waydroid shell -- am start -n com.armeafamily.shepherd.dpc/.LaunchActivity \
    --es pkg com.android.calculator2
# end the locked session
sudo waydroid shell -- am broadcast -n com.armeafamily.shepherd.dpc/.ControlReceiver \
    --es action unlock
```

Verified on the bench: device owner set; `mLockTaskModeState=LOCKED`; injecting
`KEYCODE_HOME` no longer escaped (foreground stayed on the app) — the framework
blocked it.

To remove the device owner during development (it can't be uninstalled while it
is owner, and `dpm remove-active-admin` is refused on a user build), stop the
session and delete `~/.local/share/waydroid/data/system/device_owner_2.xml`
(and `device_policies.xml`), then restart.

## Important limitation: Lock Task ⊥ multi-window

Waydroid only presents **freeform / multi-window** windows as individual Wayland
toplevels (`app_id="waydroid.<pkg>"`), which is how shepherd fullscreens and
tracks each Android app. **Lock Task Mode suppresses that** — while an app is
pinned it is *not* a `waydroid.<pkg>` toplevel (confirmed: the window exists in
Android with a ready surface, but no Sway toplevel appears; unlocking makes it
reappear).

So the DPC cannot be dropped into shepherd's current launch path — doing so
would break window-ready detection, per-app fullscreen, and exit detection.
Integrating it requires switching lock-task Android sessions to **full-UI
single-surface** presentation (`waydroid show-full-ui`, fullscreen the one
`Waydroid` surface) and tracking the session via `dumpsys` foreground activity
instead of a per-app toplevel. That is scoped future work; until then shepherd's
default `lock_down` (statusbar `send-disable-flag`, which blocks the Settings
escape) is the practical lock-in.
