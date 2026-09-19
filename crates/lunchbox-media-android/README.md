# lunchbox-media-android

Android build of `lunchbox-media`. This crate is the `cdylib` the APK loads; it
hosts the cross-platform [egui](https://github.com/emilk/egui) UI over the shared
`lunchbox-media-core` and the `lunchbox-media-app` settings layer.

Unlike the Linux binary — which `lunchboxd` spawns per activity with a
`--library` argument — there is no `lunchboxd` on Android and only one install
per device. So this app owns persistent state (the configured libraries, their
caching options, and the active selection) via `lunchbox-media-app`, and presents
a **library switcher** plus a **settings page** to manage them.

See `docs/ai/history/2026-06-27 001 lunchbox-media android architecture.md` for
the full design and roadmap.

## What works today

- Cross-platform egui UI: library switcher, settings (add / remove / reorder /
  select-active, per-library cache mode, quality, poster policy, size cap, and a
  reverse-order toggle mirroring the Linux binary's `--reverse`),
  add-library form, and a browse grid.
- Keyboard-free library adding for TVs. The add-library form's `Id`/`Label` are
  optional and auto-derived from the source (see `lunchbox-media-app`'s
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
    as the `lunchbox-http` management API) and shows its address + a QR.
    A phone on the same Wi-Fi opens the page, submits a TOML/M3U/YouTube URL, and
    the TV fills the form automatically (detecting the source kind). LAN-only and
    unauthenticated.

  Both verified on hardware. (Typing directly on the TV's on-screen keyboard is
  intentionally avoided — a `NativeActivity` can't capture soft-keyboard text
  for a D-pad-focused field; see the history doc.)
- The browse grid is the **shared `lunchbox-media-ui` poster grid** — the same
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
- Per-library **Resume playback** (default off, mirroring the Linux `--resume`):
  with it on, each item re-opens where it stopped and opening the library shows
  the shared "Continue watching" card for the last item watched. Positions live
  in `<filesDir>/resume/<library-id>.toml`; the state model and its policy are
  shared with the Linux binary (`lunchbox-media-app`'s `resume` module). Not yet
  exercised on hardware.
- Per-library **Skip sponsors** (default off, issue #159): jumps over sponsor
  reads, self-promotion, "like and subscribe", intros and end cards in YouTube
  videos, with a brief notice saying what was skipped. The categories, the
  filtering and the skip state machine are `lunchbox-media-core`'s and the
  bucket cache is `lunchbox-media-app`'s, so this behaves exactly as the Linux
  binary does; only the fetch and the cache directory
  (`<filesDir>/cache/sponsorblock/`) are local. Off means no request is made.
  Verified on a Pixel 10a, through the settings checkbox: 62s of playback
  reaches 112s of video with the toggle on and 62s with it off, and the bucket
  lands in the app's own cache.
- **Correct aspect ratio.** Under `mediacodec_embed` the decoder scales its
  output to fill the Surface it is handed, and mpv's `--keepaspect` never gets a
  look in because no pass under mpv's control draws the frame — so a
  `MATCH_PARENT` SurfaceView stretched every video to the shape of the display
  (a 16:9 video measured 26% too wide on a 2424x1080 Pixel). The Rust side works
  out the rectangle the video should occupy (`surface::fit_video`), places the
  SurfaceView there over JNI, and paints the letterbox bars itself in
  `playback.rs` — the bars have to be drawn there because `NativeActivity` hands
  the window's surface to the native renderer (`getWindow().takeSurface`), so
  nothing the Java side draws is ever shown, and the window is translucent, so
  anything unpainted shows the home screen rather than a black bar.
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
  through core's libmpv `PlayerHandle`, with a touch/keyboard control overlay
  (play/pause, ±10s, scrub, back). libmpv + ffmpeg are vendored under
  [`vendor/libmpv/`](./vendor/libmpv) and packaged into the APK. Verified on
  hardware (Pixel 10a and a Fire TV): video renders with MediaCodec hardware
  decode, audio, transport controls, and EOF.

  Video does **not** go through egui. mpv decodes straight into a `SurfaceView`
  that [`LunchboxMediaActivity`](./android/app/src/main/java/com/lunchboxos/media/LunchboxMediaActivity.java)
  puts behind the (translucent) activity window — `vo=mediacodec_embed`,
  `hwdec=mediacodec`, `--wid` — so frames stay on the GPU and SurfaceFlinger can
  put them on a hardware overlay plane. egui paints only the overlay, over
  transparency. Compositing video through egui, as the Linux binary does,
  restricts mpv to `hwdec=mediacodec-copy`, which reads every decoded frame back
  into system RAM to be re-uploaded; on a Fire TV that capped 60fps content at
  ~20fps (issue #115). See `src/surface.rs` for the Surface plumbing and the
  traps around it.
- Video caching: with a non-`Off` cache mode, a finished direct-HTTP item is
  downloaded to the per-library cache so the next play is local, with LRU
  eviction to the per-library size cap. Playback prefers a cached local copy.
- YouTube: a `youtube-playlist` source resolves into a browseable library
  (titles, thumbnails) and tapping an item plays it through libmpv, both via
  yt-dlp (bundled as `youtubedl-android`, called over JNI). Verified on hardware:
  playlist + stream resolution and video playback work, at the per-library
  quality. Two requirements are handled in `src/youtube.rs`: the player clients
  — yt-dlp's default clients serve the DASH ladder, and `android` is listed
  alongside them so DRM-protected "full episode" uploads (which the defaults
  report as "not available") fall back to the legacy progressive itag 18; this
  client selection is shared with the Linux build as
  `lunchbox_media_core::YOUTUBE_EXTRACTOR_ARGS`. The other is an in-app refresh
  of the AAR's stale bundled yt-dlp to the latest release on first launch
  (`refresh_ytdlp`). Stream + audio are resolved as separate DASH tracks
  (`StreamUrls`) and muxed at playback via `PlayerHandle::set_external_audio`. A
  transient stream error restarts the item a couple of times before giving up
  (shared `RetryBudget`). To hide the ~3s resolve, stream URLs are prefetched in
  the background (cached by watch URL): at launch every playlist is resolved,
  then the leading videos of each; while browsing, the focused tile and its two
  neighbours go first. Tapping play then reuses the cached resolution.
- The cdylib cross-compiles for `aarch64-linux-android` and exports
  `android_main` / `ANativeActivity_onCreate`.

## Not yet wired (next steps)

- `QueueAll`'s eager prefetch-at-launch (currently both non-`Off` modes cache
  after play; the size cap and LRU eviction are shared).
- JNI bridge for the connectivity policy (the `WifiOnly` poster gate). The
  storage-path bridge is wired (see the `storage` module); a SAF **tree** picker
  for USB-OTG / cloud sources is still open — the single-document SAF picker was
  rejected because it can't resolve an offline library's sibling media.

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
cargo run -p lunchbox-media-android --example desktop_preview
```

Settings persist under `<tmp>/lunchbox-media-preview/settings.toml`.

## Run the tests on a device

CI only builds the cdylib for Android; nothing runs this crate's tests there. A
`cargo-ndk`-built test binary runs straight off `/data/local/tmp` — no APK, no
signing, no emulator:

```sh
cargo ndk -t arm64-v8a test -p lunchbox-media-android --no-run
adb push target/aarch64-linux-android/debug/deps/<bin> /data/local/tmp/t
adb push vendor/libmpv/arm64-v8a/. /data/local/tmp/lib
adb shell "mkdir -p /data/local/tmp/tt && TMPDIR=/data/local/tmp/tt \
    LD_LIBRARY_PATH=/data/local/tmp/lib /data/local/tmp/t"
```

Three things are not optional:

- **`TMPDIR`.** `std::env::temp_dir()` falls back to `/tmp`, which Android does
  not have, so every test using `tempfile` fails without it.
- **`LD_LIBRARY_PATH`** at the pushed [vendored libmpv](#vendored-libmpv), for
  any binary that links it.
- **`RUSTFLAGS="-L $PWD/vendor/libmpv/<abi>"`** when testing
  `lunchbox-media-core`, which also links `-lmpv` but has no `build.rs` adding
  the vendored directory to the search path.

`lunchbox-media-app` and `lunchbox-media-core` are worth running this way too —
they are shared with the Linux front-end, and the on-device pass is what proves
the cache naming and eviction behave identically on bionic.

## Build the Android library

Requires the Rust Android target, `cargo-ndk`, and an installed NDK. The NDK is
provisioned into `/opt/android-sdk` by `./scripts/lunchbox deps install android`
(the same deps set the companion app uses); point `ANDROID_NDK_HOME` at it:

```sh
rustup target add aarch64-linux-android
cargo install cargo-ndk
export ANDROID_NDK_HOME=$ANDROID_HOME/ndk/<version>   # e.g. .../ndk/27.2.12479018

cargo ndk -t arm64-v8a build -p lunchbox-media-android --release
```

The resulting `liblunchbox_media_android.so` is what the APK packages.

## Install a released build

Released builds are published to this project's F-Droid repository and attached
to each release as an APK — see
[Installing the Android apps](../../docs/INSTALL.md#installing-the-android-apps).
App listing metadata lives in [`dist/fdroid/`](../../dist/fdroid/README.md).
Fire TV sticks are the sideload case: F-Droid has no remote-friendly interface,
so `adb install` stays the practical route there.

`lunchbox-admin apps install media` does that sideload for you, onto whatever
Android device is attached over `adb`: it downloads the version-matched signed
release asset (verifying its `.sha256`) on a packaged install, and builds the
Gradle project below in a source checkout.

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
