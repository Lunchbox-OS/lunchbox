# `shepherd-admin apps install companion|media`

*2026-08-17*

## The prompt

> make it so that the admin script can install the companion and media apps --
> by building from source for source builds, and by using the release from
> Forgejo/the F-Droid repository for package installs

Two decisions were taken with the project owner before writing anything:

1. **What "install" means.** Getting the APK is only half of it — the command
   `adb install -r`s the result onto the attached Android device, rather than
   staging a file and printing instructions.
2. **Where a packaged install gets the APK.** The **Forgejo release asset**
   matching the installed version (`releases/download/v<VERSION>/
   <artifact>_<VERSION>.apk`), verified against the `.sha256` sidecar
   `scripts/ci/upload-release-asset.sh` uploads beside every asset. The F-Droid
   repository would always carry the newest build, but reaching it means parsing
   `index-v1`/`v2`, and the version-matched asset is the one that pairs with the
   `.deb` the operator is running.

## What was added

`scripts/lib/admin.sh` gains an "Android apps" section, so both entrypoints get
it for free — `shepherd apps install …` from a checkout and `shepherd-admin apps
install …` from the `.deb`, which is what makes the source/packaged split
expressible in one place:

- `android_app_meta` — the per-app table (Gradle dir, release-asset stem,
  `applicationId`, label) for `companion` and `media`.
- `is_source_checkout` — the switch. A checkout has `Cargo.toml` +
  `companion-android/` above `scripts/lib`; the `.deb` layout
  (`/usr/lib/shepherd/lib`) does not. `--source` / `--release` override it.
- `android_build_apk` — `./gradlew --no-daemon :app:assembleDebug` in the app's
  own Gradle project. Debug-signed, because the release keystore exists only in
  CI. For `media` it pre-flights `cargo ndk` and resolves `ANDROID_NDK_HOME`
  from `$ANDROID_SDK_ROOT/ndk/*` (newest), so a missing cross-compiler is
  reported here rather than from inside Gradle's `Exec` task.
- `android_download_apk` — downloads to a per-user cache and verifies the
  `.sha256`. A missing sidecar or a checksum mismatch is fatal: this file is
  about to be installed on a family device, so an unverifiable download is a
  failure, not a warning. Cached APKs are re-verified before reuse, so a
  truncated download self-heals.
- `android_resolve_device` — installs onto the sole attached device, refuses to
  guess between several, and distinguishes "nothing attached" from
  `unauthorized` (the USB-debugging prompt). A `HOST:PORT` `--device` is
  `adb connect`ed first, which is how the Fire TV sticks are reached. It runs
  *before* the build/download, so a missing phone doesn't cost a Gradle run.
- `android_adb_install` — translates `INSTALL_FAILED_UPDATE_INCOMPATIBLE` into
  the `adb uninstall` it needs, and, for the companion app only, warns that this
  erases the admin records and claim tokens, which means factory-resetting every
  device that phone administers.

### Dependencies

The packaged path needs two things the kiosk itself does not: `curl` (fetch the
release asset) and `adb` (install it). Both are declared `Suggests:` in the
`.deb`'s control — not `Depends:` — because a device that never has a phone
plugged into it should not pull in the Android platform tools, and `apt` honours
that by not installing them. Neither is silently assumed: `android_download_apk`
calls `require_command curl` and `find_adb` searches PATH and then
`$ANDROID_SDK_ROOT`/`$ANDROID_HOME`/`/opt/android-sdk`'s `platform-tools`,
each failing with the apt line that fixes it. `sha256sum` needs no declaration
(coreutils is Essential), and TLS verification rides on `ca-certificates`, which
is Priority: important on Ubuntu.

The source path adds no new toolchain either: it is the *existing*
`shepherd deps install android` set (JDK + SDK, plus the NDK and `cargo-ndk` for
the media app's Rust cdylib), pre-flighted before Gradle runs so a missing piece
names that command instead of failing inside a Gradle task. Gradle itself
arrives through each project's wrapper.

### No root

Unlike the `steam`/`chrome` backends, these two do not touch the host — and root
would actively hurt: `adb` authorises devices against the *invoking user's*
`~/.android` key, so a `sudo` run makes an authorised phone report
`unauthorized`, and a root Gradle run leaves root-owned build output in the
checkout. `android_app_install` refuses to run under `sudo` and says why.

### Gotcha worth remembering

`android_build_apk` returns the APK path on stdout, so Gradle's own output has
to be redirected to stderr (`./gradlew … >&2`). Without that the build log is
captured into the path and `adb` reports the whole log as "No such file or
directory" — which is exactly how the first end-to-end run failed.

## Verification

Against the real release server and a phone on USB (`63251JEA305665`):

- source path — `./scripts/shepherd apps install companion` built and installed,
  upgrading the phone from 0.3.5 to 0.3.6 (`dumpsys` confirms `versionCode=306`).
- source path, Rust-native — `./scripts/shepherd apps install media` (the
  `cargo-ndk` cross-compile through `preBuild`).
- release path — `--release --version 0.3.5` downloaded and checksum-verified
  `shepherd-media_0.3.5.apk`, then produced the signature-mismatch guidance
  against the phone's debug-signed copy (as it should).
- packaged layout — `scripts/shepherd-admin` + `lib/` copied under a fake
  `/usr/lib/shepherd` with `SHEPHERD_DATA_DIR` pointing at a staged `VERSION`:
  `is_source_checkout` is false, `--source` is refused, and the default run
  downloaded and verified `shepherd-companion_0.3.6.apk`.
- failure paths — 404 for an unreleased version, a corrupted cache entry
  (re-downloaded), an unknown `--device` serial.

`shellcheck -e SC1091` clean, matching what CI runs.
