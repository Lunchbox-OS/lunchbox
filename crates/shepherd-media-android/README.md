# shepherd-media-android

Android build of `shepherd-media`. This crate is the `cdylib` the APK loads; it
hosts the cross-platform [egui](https://github.com/emilk/egui) UI over the shared
`shepherd-media-core` and the `shepherd-media-app` settings layer.

Unlike the Linux binary — which `shepherdd` spawns per activity with a
`--library` argument — there is no `shepherdd` on Android and only one install
per device. So this app owns persistent state (the configured libraries, their
caching options, and the active selection) via `shepherd-media-app`, and presents
a **library switcher** plus a **settings page** to manage them.

See `docs/ai/history/2026-06-27 001 shepherd-media android architecture.md` for
the full design and roadmap.

## What works today

- Cross-platform egui UI: library switcher, settings (add / remove / reorder /
  select-active, per-library cache mode, quality, poster policy, and size cap),
  add-library form, and a placeholder browse grid.
- Settings persisted as TOML to the app's private storage.
- The cdylib cross-compiles for `aarch64-linux-android` and exports
  `android_main` / `ANativeActivity_onCreate`.

## Not yet wired (next steps)

- Resolving a `LibrarySource` into a `Library` (HTTP/SAF file reads, yt-dlp via
  youtubedl-android).
- libmpv-backed `PlayerHandle` and embedded playback (today a `StubPlayer`).
- JNI bridges for the SAF file picker, connectivity policy, and storage paths.

## Develop on the host

The same `MediaApp` runs natively for fast iteration — no device or emulator:

```sh
cargo run -p shepherd-media-android --example desktop_preview
```

Settings persist under `<tmp>/shepherd-media-preview/settings.toml`.

## Build the Android library

Requires the Rust Android target, `cargo-ndk`, and an installed NDK:

```sh
rustup target add aarch64-linux-android
cargo install cargo-ndk
export ANDROID_NDK_HOME=$ANDROID_HOME/ndk/<version>

cargo ndk -t arm64-v8a build -p shepherd-media-android --release
```

The resulting `libshepherd_media_android.so` is what the APK packages.

## Build the APK

The Gradle project under [`android/`](./android) is a pure-`NativeActivity` app
(no Java/Kotlin): its `preBuild` runs the `cargo-ndk` cross-compile and stages
the `.so` into `jniLibs`.

```sh
cd android
ANDROID_NDK_HOME=$ANDROID_HOME/ndk/<version> ./gradlew assembleDebug
```

This produces `android/app/build/outputs/apk/debug/app-debug.apk` with the
`arm64-v8a` `.so` packaged inside. Use `assembleRelease` for an (unsigned)
release build.

`local.properties` must point at the SDK (`sdk.dir=$ANDROID_HOME`), or set
`ANDROID_HOME` in the environment. Opening `android/` in Android Studio also
works and generates the wrapper automatically.
