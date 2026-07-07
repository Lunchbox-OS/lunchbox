# shepherd-media-android: armeabi-v7a support for 32-bit Fire TV

## Prompt

> connect to the Fire TV at 192.168.0.39 and deploy shepherd-media-android to it

Then, after discovering the ABI mismatch below:

> add armeabi-v7a support; keep it in this branch

## Context

Deploying to the Fire TV at `192.168.0.39` (`adb connect …:5555`) surfaced a
device that is **32-bit only**: model `AFTHA004` ("hazel") reports
`ro.product.cpu.abilist = armeabi-v7a,armeabi` with no `arm64-v8a`. The app was
built `arm64-v8a`-only, so `adb install` failed with
`INSTALL_FAILED_NO_MATCHING_ABIS (res=-113)`.

## What was done

Added `armeabi-v7a` as a second packaged ABI so the same APK installs on both
32-bit sticks and 64-bit phones/TVs.

1. **Vendored the 32-bit native libs.** The current `arm64-v8a/` libs byte-match
   the `dev.jdtech.mpv:libmpv` **1.0.0** AAR (mpv v0.41.0). That same AAR ships
   `armeabi-v7a` (and `x86`/`x86_64`). Extracted the `armeabi-v7a` `jni/` libs
   into `vendor/libmpv/armeabi-v7a/`, mirroring the arm64 file set exactly
   (9 `.so`s; dropped the AAR's `libplayer.so`, which this app doesn't use).

2. **`build.rs`** — mapped `target_arch = "arm"` → `armeabi-v7a` for the link
   search path.

3. **`android/app/build.gradle.kts`** — `rustAbis = listOf("arm64-v8a", "armeabi-v7a")`
   (drives both the packaged `abiFilters` and the `cargo-ndk` cross-compile).

4. **`Cargo.toml`** — the real blocker. `libmpv2-sys` by default copies
   `pregenerated_bindings.rs`, whose `__bindgen_test_layout_*` asserts bake in a
   **64-bit** struct layout (e.g. `mpv_stream_cb_info` = 48 bytes for 6 pointers).
   On 32-bit armv7 that struct is 24 bytes, so the const assertions fail to
   compile (`attempt to compute 24 - 48, which would overflow`). Fix: force
   `libmpv2-sys` to regenerate with bindgen instead. It isn't a direct dep, so it
   was added as an **Android-only** direct dep with `features = ["use-bindgen"]`
   under `[target.'cfg(target_os = "android")'.dependencies]`; feature
   unification then turns bindgen on for the transitively-used copy, for Android
   builds only. The host build (desktop preview, CI) keeps the pregenerated path.

   bindgen needs `libclang` on the build host (present: `libclang-21`).
   `cargo-ndk` sets `BINDGEN_EXTRA_CLANG_ARGS_<triple>` (sysroot + `--target`) so
   bindgen picks the correct per-ABI data model automatically.

Also installed the `armv7-linux-androideabi` Rust target.

## Build / deploy

```sh
export ANDROID_HOME=/opt/android-sdk
export ANDROID_NDK_HOME=$ANDROID_HOME/ndk/27.2.12479018
export LIBCLANG_PATH=/usr/lib/x86_64-linux-gnu   # for bindgen
rustup target add armv7-linux-androideabi
cd crates/shepherd-media-android/android && ./gradlew assembleDebug
adb -s 192.168.0.39:5555 install -r app/build/outputs/apk/debug/app-debug.apk
```

## Verification

- APK packages both `lib/arm64-v8a/` and `lib/armeabi-v7a/`.
- Installed to the Fire TV (`AFTHA004`) → `Success`.
- Launched: `NativeActivity` reaches Resumed, cold launch ~1.1s, GL surface up,
  no `UnsatisfiedLinkError` / `dlopen` failure / FATAL in logcat — i.e. the
  32-bit libmpv + cdylib load and run.
- Host `cargo check -p shepherd-media-android` still builds (pregenerated path
  unaffected); `cargo fmt --all -- --check` clean.

## Notes / not done

- Playback wasn't exercised end-to-end on the 32-bit stick (launch + native-load
  verified). Worth a follow-up smoke test of actual video on the AFTHA004.
- Only the debug APK was built/installed here.

## Follow-up: two TV UX fixes (same session)

> pressing the back button on the library screen should close the app, and there
> appears to be no default selection if no libraries are present, making it
> impossible to add the first one

Both surfaced while testing on the AFTHA004 (D-pad only, no pointer).

1. **No focus on the empty library switcher.** The switcher is excluded from the
   generic per-frame focus bootstrap because it "focuses its first library"
   explicitly — but the empty branch (`libraries.is_empty()`) rendered the
   "➕ Add a library" button and focused nothing, so the remote's center button
   had no target and the first library could never be added. Fix: focus that
   button when nothing else is (`src/ui.rs`, switcher's empty branch).

2. **BACK on the top-level switcher did nothing; should exit.** The back handler
   returned `None` for `Screen::Switcher`. `ViewportCommand::Close` was tried
   first but doesn't reliably finish a `NativeActivity` (winit stops its loop;
   Android keeps the activity — observed flaky: sometimes the process was killed,
   sometimes nothing happened). Replaced with a deterministic `Activity.finish()`
   over JNI: new `src/exit.rs` module (mirrors `insets`/`storage`: holds the
   activity pointer set from `android_main`, attaches to the JVM, calls
   `finish()`). Verified 3/3: BACK on the switcher moves the top activity from
   our `NativeActivity` to the Fire TV launcher. Note `finish()` ends the app's
   UI (returns to home) but leaves the process cached — expected Android
   behavior, not a leak, and cleaner than `Close` hard-killing the process.
