# 2026-06-28 — Android Phase 4: kiosk lock-in + app provisioning runbook

Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/2>
Prior: [2026-06-28 007 android-in-kiosk-verification.md](2026-06-28%20007%20android-in-kiosk-verification.md)

## Prompt

> yes, keep going  (→ start Phase 4)

Phase 4 = child lock-in + the GAPPS-dependent wishlist apps. Lock-in is the
shepherd-codeable, security-relevant part and is implemented + verified here.
The app provisioning is external operator work and is a runbook.

## Part A — Kiosk lock-in (implemented + verified)

### What actually contains a child here (empirical, on real Waydroid)

I probed the live Android-13 Waydroid session in multi-window mode:

- **Each app is its own Sway toplevel** (`waydroid.<pkg>`); there is no separate
  Android status/nav-bar window on screen.
- **HOME is only a *soft* escape.** Injecting `KEYCODE_HOME` made the app's
  toplevel disappear and `com.android.launcher3` (Android home) the foreground
  activity — but the launcher is **not** presented as a Sway toplevel. So in
  shepherd's architecture the app window vanishing makes the window-watch task
  emit `Exited`, shepherd ends the session, and the child lands back on
  **shepherd's own launcher grid** — they never see the Android home. Acceptable
  (it behaves like quitting the activity).
- **The real escape is the notification shade / quick settings**, which can
  reach `com.android.settings`.

### The fix — no Device Owner app needed

Android's `cmd statusbar send-disable-flag` takes
`home recents statusbar-expansion notification-peek search`. Applying them set
`mDisabled1=0x3210000` and **`expand-settings` no longer opened the shade**
(foreground stayed on the app). This blocks the dangerous route with *no*
custom DPC / device-owner provisioning. Caveat: it disables nav-bar *buttons*,
not an injected `KEYCODE_HOME` — but HOME is only the soft escape above, so
that's fine.

### Implementation

- **Helper** (`shepherd-waydroid-helper`): new `lock-down` subcommand →
  `waydroid shell cmd statusbar send-disable-flag <fixed flags>`. No arguments,
  flags hardcoded (no caller input). Same polkit action gates it.
- **`waydroid::lock_down`** → `pkexec helper lock-down` (best-effort).
- **`[service.waydroid] lock_down`** (default **true**) →
  `WaydroidConfig.lock_down`; threaded through `configure_waydroid` into the
  adapter. `spawn_android` applies it **per launch** right after `WindowReady`
  (re-applied each launch since the flags reset on a SystemUI restart).
- `config.example.toml`, helper README updated.

### Verification (end-to-end, real Waydroid)

`test-waydroid.sh` now asserts it: after a real launch through the adapter,
`dumpsys statusbar` shows `mDisabled1=0x3210000`:

```
[OK] Android app launched, window present: waydroid.com.android.calculator2
[OK] Android session stopped, window gone, Exited emitted
[OK] lock-down active: mDisabled1=0x3210000 (shade/nav disabled)
```

So the production path `spawn_android → waydroid::lock_down → pkexec → helper →
cmd statusbar` genuinely hardens the session. `fmt` / `clippy -D warnings` /
`cargo test --all-targets` green (320 passed, 0 failed).

### What lock-down does NOT cover (and the gold-standard option)

- **A kiosk app launching *another* app** (e.g. a link opening a browser) makes
  a new `waydroid.<otherpkg>` toplevel. Sway still fullscreens it (the
  `for_window [app_id="^waydroid\..*"]` rule), but shepherd's session tracks
  only the original package. For untrusted-content apps this is a gap.
- **The gold standard** is Lock Task Mode via a **Device Owner** DPC app
  (`dpm set-device-owner` over root ADB, then `setLockTaskPackages` +
  `startLockTask`), which also pins to an app allowlist and blocks other-app
  launches. That's a separate Android app to build/ship and is **deferred** —
  `lock_down` covers the primary child-kiosk threat (reaching Settings) without
  it. Documented here so it's a known, scoped follow-up.

## Part B — GAPPS + app provisioning runbook (operator, not launcher code)

The wishlist apps are external provisioning — Google sign-in, a paid purchase,
ARM translation — that shepherd can't (and shouldn't) automate. Runbook:

1. **GApps image + Play certification** (needed by M365/Duolingo/Bedrock for
   sign-in):
   ```sh
   sudo waydroid init -s GAPPS        # or re-init; downloads the GApps image
   # open Play Store once, get the Android ID, register at
   # https://www.google.com/android/uncertified, wait, restart the session
   ```
2. **x86 → ARM translation** (this host is x86_64; many APKs incl. Minecraft
   Bedrock have ARM-only native libs): install libndk (AMD) / libhoudini (Intel)
   via the third-party `casualsnek/waydroid_script`. Pick one.
3. **Install an app**: `waydroid app install file.apk` (or via Play once
   certified). Find the package name with `waydroid app list` and put it in the
   entry's `package_name`.
4. **Per-app notes** (from scoping, [2026-06-28 001]):
   - *Khan Academy Kids* — lightest; sideloads, often works without full Google
     sign-in. Best first real app.
   - *Microsoft 365* — has an x86 build; sign-in/sync wants GApps.
   - *Duolingo* — sideloadable; login usually forces Google sign-in → GApps.
   - *Minecraft Bedrock* — paid + Play-only license (GApps + certification +
     purchase), ARM translation on x86, **and Waydroid doesn't support Nvidia
     GPUs**. Most friction; verify GPU first.
5. **Lock-in for these**: `lock_down` (Part A) covers the Settings escape. If an
   app can launch other apps and that matters, build the Device Owner DPC
   (deferred above).

This runbook is environment- and account-specific and was **not** executed on
the dev box (no Google account / paid Minecraft / ARM layer here, and the VM is
software-rendered). It belongs in operator docs, not the codebase.

> **Update 2026-07-12 ([2026-07-12 001]):** steps 1's GAPPS init and the DPC
> device-owner part *were* since prototyped on the dev box — GAPPS init works
> (note: `init -f` does **not** wipe `~/.local/share/waydroid/data`), and
> `dpm set-device-owner` succeeds on a fresh account-free GAPPS instance. Still
> unrun here: Google sign-in, Play certification, paid/ARM apps (need an account).

## Status

Issue #2 is functionally complete for the trusted-app child-kiosk case:
scoping → spike → config → runtime → privileged helper → in-kiosk verification →
preboot → **kiosk lock-in (verified)**. Remaining are operator provisioning
(Part B runbook) and the optional Device Owner Lock Task hardening for
untrusted-content apps.
