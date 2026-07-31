# shepherd-dpc

A minimal Android **Device Policy Controller** for Waydroid kiosk lock-in. Set
as **device owner** (no Google account needed on a fresh Waydroid), it pins a
kiosk app in **Lock Task Mode** — blocking HOME, Recents, the status bar, and
launching other apps at the framework level. This is the gold-standard
containment, stronger than the statusbar-flag `lock_down` shepherd applies by
default.

> **Status: integrated.** shepherd drives this DPC as the `[service.waydroid]
> lock_mode = "locktask"` launch path — it presents the app as the single full-UI
> `Waydroid` surface and pins it via the DPC (see [Lock Task ⊥ multi-window](#important-limitation-lock-task--multi-window)
> for how that constraint was resolved). The default remains the softer
> statusbar-flag `lock_mode = "statusbar"`; `locktask` is opt-in per the managed
> Android configuration. The apk ships in the `.deb` and is set as device owner by
> `shepherd-admin apps install android`.

## Build

No Gradle — a plain SDK pipeline (`aapt2 → javac → d8 → zipalign → apksigner`):

```sh
ANDROID_SDK_ROOT=/opt/android-sdk ./build.sh    # -> shepherd-dpc.apk
```

The signing keystore (`dpc.keystore`, gitignored) is generated on first build
and **kept** — once the app is device owner it can only be updated with the same
key. Build artifacts and the APK are gitignored.

### How the apk reaches a device

Nothing builds this for you — that key rule is why. The apk is *staged*, then
*provisioned*:

| | staged by | from |
|---|---|---|
| `.deb` | the release job builds + signs it with the org key, then `install_system` stages it | `/usr/share/shepherd/shepherd-dpc.apk` |
| source | `./scripts/shepherd install dpc` (also part of `install all`) — build it first, or it warns and skips | same path |
| checkout, run in place | nothing; `shepherd-admin` reads it out of the tree | `dpc-waydroid/shepherd-dpc.apk` |

`shepherd-admin apps install android` then `pm install`s whichever it finds and
sets the device owner. Missing apk = it tells you to build it and stops. The
`shepherd-dpc.apk.version` sidecar `build.sh` writes is how a kiosk (with no
`aapt`) knows whether the staged apk is newer than the installed DPC.

`shepherd package deb` **refuses to build without the apk**, since the resulting
package would silently lose `lock_mode = "locktask"` on every device it installs
and nothing would say so until an operator ran `apps install android`. Pass
`--allow-missing-dpc` for a deliberate DPC-less package (CI's packaging smoke
build does; the release job fails instead, unless its `allow_missing_dpc` input
is set).

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
# install the DPC. `waydroid app install` is the documented path, but it can
# silently no-op in a headless/scripted session — the reliable method is to push
# the APK into the container's /data and pm install it:
sudo install -D -m 0644 shepherd-dpc.apk \
    ~/.local/share/waydroid/data/local/tmp/dpc.apk        # == container /data/local/tmp
sudo waydroid --details-to-stdout shell -- pm install -r -g /data/local/tmp/dpc.apk
# set it as device owner (needs root). The one hard rule: NO Google account yet
# (accounts=0). A GAPPS-provisioned device is fine — device_provisioned=1 does
# NOT block this.
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

**On GAPPS specifically** (prototyped 2026-07-12, see
`docs/ai/history/2026-07-12 001 …`): device owner works the same as on vanilla,
but two things differ. (1) Play Protect delays `pm install` registration ~15 s —
`Success` prints immediately, but `pm list packages` won't show the package for
up to ~15 s (vanilla is instant). (2) `waydroid init -f -s GAPPS` does **not**
wipe `~/.local/share/waydroid/data`; for a truly clean device add
`sudo rm -rf ~/.local/share/waydroid/data`. Google sign-in *after* device owner,
and Play/paid-app behaviour on a managed uncertified instance, remain untested.

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

So the DPC cannot be dropped into shepherd's *windowed* launch path — doing so
would break window-ready detection, per-app fullscreen, and exit detection.
**Resolved** (2026-07-12): the `lock_mode = "locktask"` path in
`crates/shepherd-host-linux/src/adapter.rs` gives lock-task sessions a distinct
presentation — `waydroid show-full-ui` presents the single `Waydroid` surface
(fullscreened by a `for_window [app_id="Waydroid"]` rule), the app is pinned via
the DPC, and stop sway-`kill`s that surface (a graceful close is ignored by the
renderer) so the window-watch still emits `Exited`. `preboot` forces
`multi_windows` off for locktask. The softer statusbar `lock_mode = "statusbar"`
remains the default; see `docs/ai/history/2026-07-12 002 …`.
