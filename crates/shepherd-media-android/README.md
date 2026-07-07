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
  select-active, per-library cache mode, quality, poster policy, size cap, and a
  reverse-order toggle mirroring the Linux binary's `--reverse`),
  add-library form, and a browse grid.
- Keyboard-free library adding for TVs. The add-library form's `Id`/`Label` are
  optional and auto-derived from the source (see `shepherd-media-app`'s
  `LibrarySource::suggested_id`/`suggested_label`). Two no-type paths cover the
  two kinds of source:
  - **📁 Browse device…** opens an in-app, D-pad-navigable file browser
    (`storage` module + the `FilePicker` screen) for on-device `.toml`/`.m3u`
    sources. The browser hands the resolver a real filesystem path, so an offline
    library's relative media resolves against its own directory. Reading shared
    storage needs "All files access" (`MANAGE_EXTERNAL_STORAGE`), requested via
    the system settings screen; it reaches internal storage and SD cards, not
    USB-OTG (SAF-only).
  - **📱 Add from phone…** (for URL sources) starts a tiny LAN web server
    (`handoff` module + the `PhoneHandoff` screen — `axum`, the same server stack
    as the `shepherd-http` management API) and shows its address + a QR.
    A phone on the same Wi-Fi opens the page, submits a TOML/M3U/YouTube URL, and
    the TV fills the form automatically (detecting the source kind). LAN-only and
    unauthenticated.

  Both verified on hardware. (Typing directly on the TV's on-screen keyboard is
  intentionally avoided — a `NativeActivity` can't capture soft-keyboard text
  for a D-pad-focused field; see the history doc.)
- The browse grid is the **shared `shepherd-media-ui` poster grid** — the same
  responsive poster-tile view the Linux binary uses, so the two front-ends stay
  in sync. The Android app supplies the items, poster bytes, and focus input;
  the crate renders the tiles.
- TV remote / D-pad navigation (Fire TV, Google TV): the Android D-pad maps to
  egui arrow-key focus movement; the app keeps a widget focused at all times
  (auto-focusing the first control on each screen), draws a prominent focus
  highlight, activates on the center button (Enter), and treats BACK
  (`BrowserBack`) as "navigate up". In playback the remote seeks (◄/►), toggles
  play/pause (center), and leaves (BACK). Verified on hardware.
- Settings persisted as TOML to the app's private storage.
- Source resolution (`resolve` module): local/`file://` TOML, HTTP(S) TOML, and
  `.m3u`/`.m3u8` (local or HTTP) are parsed into a `Library` on a worker thread,
  and the grid lists the real items. `content://` SAF and YouTube sources report
  a typed "unsupported yet" error.
- Poster thumbnails in the grid: loaded and decoded (JPEG/PNG/WebP) on worker
  threads and uploaded as egui textures, honoring `PosterPolicy` (`Never` skips;
  `WifiOnly` currently behaves like `Always` pending the connectivity bridge).
  Remote posters are cached on disk (6 h TTL, stale-as-offline-fallback) so they
  persist across launches.
- libmpv playback: tapping an item resolves its platform source and plays it
  through core's libmpv `PlayerHandle`, composited into the eframe GL surface
  with a touch/keyboard control overlay (play/pause, ±10s, scrub, back). libmpv
  + ffmpeg are vendored under [`vendor/libmpv/`](./vendor/libmpv) and packaged
  into the APK. Verified on hardware (Pixel 10a): video renders (incl.
  MediaCodec H.264 hardware decode) with audio, transport controls, and EOF.
- Video caching: with a non-`Off` cache mode, a finished direct-HTTP item is
  downloaded to the per-library cache so the next play is local, with LRU
  eviction to the per-library size cap. Playback prefers a cached local copy.
- YouTube: a `youtube-playlist` source resolves into a browseable library
  (titles, thumbnails) and tapping an item plays it through libmpv, both via
  yt-dlp (bundled as `youtubedl-android`, called over JNI). Verified on hardware:
  playlist + stream resolution and video playback work. Two requirements are
  handled in `src/youtube.rs`: the `android_vr` player client (serves video
  formats without a PO token; the default `android` client returns audio-only),
  and an in-app refresh of the AAR's stale bundled yt-dlp to the latest release
  on first launch (`refresh_ytdlp`). Stream + audio are resolved as separate
  DASH tracks (`StreamUrls`) and muxed at playback via
  `PlayerHandle::set_external_audio`.
- The cdylib cross-compiles for `aarch64-linux-android` and exports
  `android_main` / `ANativeActivity_onCreate`.

## Not yet wired (next steps)

- `QueueAll`'s eager prefetch-at-launch (currently both non-`Off` modes cache
  after play; the size cap and LRU eviction are shared).
- JNI bridge for the connectivity policy (the `WifiOnly` poster gate). The
  storage-path bridge is wired (see the `storage` module); a SAF **tree** picker
  for USB-OTG / cloud sources is still open — the single-document SAF picker was
  rejected because it can't resolve an offline library's sibling media.
- Per-library quality applied to the libmpv `ytdl-format` (currently a single
  default is set at startup).

## Vendored libmpv

`vendor/libmpv/<abi>/` holds the prebuilt `libmpv.so` plus its ffmpeg
dependencies, extracted from the `dev.jdtech.mpv:libmpv` AAR (Maven Central,
version `1.0.0` — mpv v0.41.0). `build.rs` adds the ABI's directory to the link
search path so the `-lmpv` from `libmpv2-sys` resolves, and the Gradle project
packages the same `.so`s into the APK's `jniLibs`. Two ABIs are vendored today:

- `arm64-v8a/` — modern phones and 64-bit TVs.
- `armeabi-v7a/` — 32-bit-only Fire TV sticks (e.g. AFTHA004 "hazel", which
  reports no `arm64-v8a`). The 32-bit target needs `libmpv2-sys`'s bindgen path
  rather than its pregenerated (64-bit-layout) bindings; that's forced on for
  Android in `Cargo.toml` (`libmpv2-sys` with `use-bindgen`), which needs
  `libclang` on the build host. `cargo-ndk` sets `BINDGEN_EXTRA_CLANG_ARGS_<triple>`
  so bindgen picks the right per-ABI data model.

To add another ABI (e.g. `x86_64/` for the emulator): unzip that ABI's `jni/`
libs from the AAR into a sibling dir — copy the same file set as an existing ABI
(drop the AAR's `libplayer.so`, which this app doesn't use) — add the ABI to
`rustAbis` in `android/app/build.gradle.kts`, and map its Rust `target_arch` to
the dir name in `build.rs`.

## Develop on the host

The same `MediaApp` runs natively for fast iteration — no device or emulator:

```sh
cargo run -p shepherd-media-android --example desktop_preview
```

Settings persist under `<tmp>/shepherd-media-preview/settings.toml`.

## Build the Android library

Requires the Rust Android target, `cargo-ndk`, and an installed NDK. The NDK is
provisioned into `/opt/android-sdk` by `./scripts/shepherd deps install android`
(the same deps set the companion app uses); point `ANDROID_NDK_HOME` at it:

```sh
rustup target add aarch64-linux-android
cargo install cargo-ndk
export ANDROID_NDK_HOME=$ANDROID_HOME/ndk/<version>   # e.g. .../ndk/27.2.12479018

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
