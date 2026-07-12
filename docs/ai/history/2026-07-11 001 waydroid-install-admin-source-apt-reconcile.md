# Reconcile the Waydroid install/admin scripts with the source+apt model

**Branch:** `u/albert/2/android-activity` · **Date:** 2026-07-11

## Prompt

After merging `origin/main` into the Android-activity-kind branch, the follow-up
ask was: "how about for the installation and admin scripts, which are now both
source and apt aware".

## Context

`main` (via #82, the binary-releases work) reshaped the shell tooling so a system
install can happen two ways from one source of truth:

- `install_system()` in `scripts/lib/install.sh` is the single list of
  host-global, DESTDIR-safe components. Both `install_all` (from source) and
  `package_deb` (`scripts/lib/package.sh`, `.deb`/apt staging) call it.
- Post-install, host-mutating, per-user, and heavy optional-integration tasks
  live in `scripts/lib/admin.sh`, exposed by `scripts/shepherd` (source) and the
  packaged `scripts/shepherd-admin` CLI (no repo needed). Steam/Chrome are
  provisioned on demand via `shepherd-admin apps install steam|chrome`.

The branch's Waydroid work predated all of this, so it was source-only:
`install_waydroid()` staged the helper + polkit **and** did host mutation
(group/usermod/polkit reload) in one function, reachable only via
`shepherd install waydroid` (which needs the repo for `dist/polkit/…`). Gaps:

1. The `.deb`/apt path never shipped the helper — not in `install_system`, and
   `shepherd-waydroid-helper` isn't in `SHEPHERD_BINARIES` (though
   `cargo build --release` builds it anyway). Apt-only boxes had no path to the
   Android activity kind at all.
2. No `shepherd-admin` provisioning task for Android/Waydroid, unlike Steam/Chrome.
3. Deps weren't apt-aware for the Waydroid engine.

## Decision

Chosen model: **Steam-style opt-in.** Ship the helper binary + polkit assets in
the base install (so apt boxes have them, DESTDIR-safe), and defer all host
mutation — the `shepherd-waydroid` group, the Waydroid engine, `waydroid init` —
to an opt-in `shepherd-admin apps install android`. Keeps every install lean and
matches how the heavy Steam backend is provisioned. The Lock Task DPC
(`dpc-waydroid/`) stays out: it is a verified building block but is explicitly
NOT wired into shepherd by default (incompatible with current Android windowing).

## Changes

- `scripts/lib/install.sh`: split `install_waydroid()` into
  `install_waydroid_assets()` (files only, DESTDIR-safe) and
  `provision_waydroid_host()` (group + membership + polkit reload, no-op under
  DESTDIR). `install_waydroid()` now composes the two (unchanged behavior for
  `shepherd install waydroid` and `setup-waydroid-dev.sh`). `install_system()`
  now calls `install_waydroid_assets` so the `.deb` and `install all` both ship
  the helper + polkit rule (inert until the group exists).
- `scripts/lib/admin.sh`: new `apps install android [USER]` — verifies the
  shipped helper, calls the shared `provision_waydroid_host`, and prints the
  Waydroid-engine install steps (third-party repo + `waydroid init`, ~2.4 GB).
- `scripts/lib/package.sh`: waydroid polkit rule added to `conffiles`; control
  Description + postinst guidance mention the Waydroid backend / `apps install
  android`.
- Docs/usage: `scripts/shepherd`, `scripts/shepherd-admin`, `scripts/README.md`,
  `docs/INSTALL.md` updated to list `android` and document both the source
  (`shepherd install waydroid`) and packaged (`shepherd-admin apps install
  android`) paths.

Verified: `bash -n` + `shellcheck -e SC1091` (the exact CI invocation) clean;
`shepherd-admin apps` dispatch smoke-tested; workspace still resolves and
`shepherd-host-linux` compiles.
