# 2026-06-28 — Android: Device Owner DPC + Lock Task (built, with a blocking finding)

Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/2>
Prior: [2026-06-28 008 android-phase4-lockin-and-app-runbook.md](2026-06-28%20008%20android-phase4-lockin-and-app-runbook.md)

## Prompt

> build the app. there should already be an Android SDK installed from a
> previous effort.

Build the Device Owner DPC app for true Lock Task Mode — the gold-standard
kiosk lock-in deferred in Phase 4a.

## Toolchain

Found the SDK at `/opt/android-sdk` (build-tools 35.0.0, platform android-35,
platform-tools, NDK) + system Java 21. No Gradle, so the app builds with a plain
`aapt2 → javac → d8 → zipalign → apksigner` pipeline (`dpc-waydroid/build.sh`).

## What was built

`dpc-waydroid/` — a ~13 KB DPC APK:
- `AdminReceiver` (DeviceAdminReceiver), `device_admin.xml` policies.
- `LaunchActivity` — allowlists a target package for Lock Task and launches it
  with `ActivityOptions.setLockTaskEnabled(true)`, pinning even apps that don't
  call `startLockTask()`.
- `ControlReceiver` — clears the allowlist to end a locked session.
- `QUERY_ALL_PACKAGES` so the DPC can resolve launch intents for arbitrary kiosk
  packages (Android 11+ package visibility otherwise returns null — this bit me
  mid-build).

## Verified on real Waydroid

- APK builds, signs (stable keystore), installs.
- `dpm set-device-owner com.armeafamily.shepherd.dpc/.AdminReceiver` → **Success** (works on
  a fresh, account-free Waydroid).
- DPC-launched app → **`mLockTaskModeState=LOCKED`**.
- Injected `KEYCODE_HOME` **no longer escaped** — foreground stayed on the app
  (vs. the statusbar-only `lock_down`, where a HOME *keyevent* still fired). So
  Lock Task delivers the stronger, framework-level containment.

Build gotchas worth recording: XML comments can't contain `--`; the keystore
must live outside the wiped `build/` dir or every build re-keys and updates fail
with a signature mismatch; `waydroid app install` won't update a device-owner
app (push + `pm install -r`, or clear the device-owner state file); pass dashed
`am`/`dpm` args through `waydroid shell -- …` (its argparse otherwise eats `-n`).

## The blocking finding: Lock Task ⊥ Waydroid multi-window

Waydroid presents only **freeform / multi-window** windows as individual Wayland
toplevels (`app_id="waydroid.<pkg>"`) — the basis of shepherd's per-app
fullscreen and tracking. **Lock Task Mode suppresses that.** While pinned, the
app's window exists in Android (surface ready, `mode=freeform`, foreground) but
**no Sway toplevel appears**; clearing Lock Task makes the toplevel reappear
immediately (confirmed both directions).

Implication: the DPC **cannot be dropped into shepherd's current launch path** —
it would break `WindowReady` detection, per-app fullscreen, and the window-watch
exit path. So it is **committed as a standalone, verified capability but not
wired into shepherd by default**. The shipped `lock_down` (statusbar
`send-disable-flag`, Phase 4a) remains the practical default; it blocks the
dangerous Settings escape and keeps windowing intact.

## To actually adopt Lock Task later (scoped)

Switch lock-task Android sessions to **full-UI single-surface** presentation:
`waydroid show-full-ui`, fullscreen the single `Waydroid` surface in Sway, and
track the session via `dumpsys` foreground activity (a B2-style poll) instead of
a per-app `waydroid.<pkg>` toplevel. That's a real windowing change, out of
scope here. The DPC app is ready for it.

## Status

The DPC app the user asked for is built, verified (device owner + Lock Task +
HOME blocked), and committed with provisioning docs. The honest blocker — Lock
Task vs. Waydroid multi-window — is documented with a concrete path to resolve
it. shepherd's behavior is unchanged (no regression); `lock_down` stays the
default lock-in.
