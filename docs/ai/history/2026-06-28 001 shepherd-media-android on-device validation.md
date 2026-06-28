# shepherd-media-android: on-device validation

## Prompt

> Validate the shepherd-media-android app. You're now on an environment with a
> dev phone you can take full control over via adb, and you can use
> <https://www.youtube.com/playlist?list=PL6D326BFD2E6696FC> as a YouTube
> playlist library when needed. This is a phone, so a 480p cache is fine.

Goal: exercise the two caveats the crate README / architecture doc flagged as
**unverified on hardware** —

1. libmpv on-device video rendering, and
2. the YouTube yt-dlp-over-JNI execution path —

plus the surrounding browse / cache pipeline, on a real device.

## Environment

- Device: Pixel 10a (`stallion`), Android 16 (SDK 36), arm64-v8a — matches the
  single packaged ABI.
- Toolchain installed for the build: NDK `27.2.12479018` (the version the design
  doc recorded), `aarch64-linux-android` rustup target, `cargo-ndk` 4.1.2.
  SDK already present at `/opt/android-sdk`.
- APK built with `./gradlew assembleDebug` (cargo-ndk cross-compile → AGP). The
  APK packages `libmpv.so` + ffmpeg, `libshepherd_media_android.so`, and the
  yt-dlp Python runtime (`libpython.zip.so` + DEX).

### Test rig

- Two libraries injected by writing `files/settings.toml` via
  `run-as com.armeafamily.shepherdmedia` (debug build) — avoids driving the
  egui add-library form through the soft keyboard:
  - **HTTP Test Library** — an `http-toml` library served from the host over
    `adb reverse tcp:8000`, with two 480p H.264+AAC test clips
    (`ffmpeg testsrc2` + sine, ~3.5 MB each) and a JPEG poster. Cache mode
    `queue-after-play`, quality `480p`, 50 MiB cap.
  - **YouTube Playlist** — `PL6D326BFD2E6696FC` (the provided playlist).
- The app drove via `adb shell input tap/keyevent` + `screencap`.

## Results

### libmpv on-device playback — VERIFIED WORKING

- HTTP-TOML library resolved over `adb reverse` (server logged
  `GET /lib.toml` + two `GET /poster.jpg`); grid showed both items with decoded
  JPEG posters → poster decode + egui texture upload works on device.
- Tapping Play streamed `clip480.mp4`: **video rendered** full-screen
  (testsrc2 pattern with its embedded timecode advancing 1.5 s → 7 s → …),
  composited into the eframe GL surface, with audio (`AHal::Stream: start
  stream primary-playback`). Clip ran the full ~20 s to EOF (the two streaming
  vs. cache-download `GET`s were 21 s apart) and returned to the grid.
- Controls verified via keyboard (Space / ArrowRight, which the playback view
  handles directly): **pause** froze the timecode at frame 47 across a 4 s gap
  and flipped the button to "▶ Play"; **seek** jumped position + moved the
  scrubber knob.
- NOTE: tapping the on-screen control buttons near the very bottom of the
  screen (e.g. the Pause button) lands in the system gesture-nav zone and fires
  Android Back, leaving playback. This is a test-harness artifact (overlay
  buttons sit low in landscape), not an app crash — worth keeping in mind for
  the bottom control bar's height/insets.

### Poster + video caching — VERIFIED WORKING

- `files/cache/posters/<hash>.bin` (7760 B = the poster) written on first grid
  view.
- `files/cache/videos/test-http/<hash>.mp4` (3 512 564 B = exact clip size)
  written after playback finished (queue-after-play).
- Replaying the cached clip served it from the local copy — **no new HTTP GET**
  — confirming `cached_path` is preferred. Caches survive the relaunch.

### YouTube yt-dlp-over-JNI — WAS A HARD CRASH, NOW FIXED

On the first attempt, opening the YouTube library **aborted the process**
(SIGABRT) on the resolver worker thread:

```
JNI DETECTED ERROR IN APPLICATION: JNI NewStringUTF called with pending exception
java.lang.ClassNotFoundException: Didn't find class
"com.yausername.youtubedl_android.YoutubeDL"
  ... at shepherd_media_android::youtube::jni_impl::JniYtDlp::run
```

This is exactly the gotcha the source comment predicted: on a Rust-spawned
worker thread attached to the JVM, the implicit `FindClass` behind a class-name
lookup resolves against the **bootstrap** classloader, which can't see the app's
DEX classes → `ClassNotFoundException`; the next `NewStringUTF` then aborts via
CheckJNI (a recoverable error turned into a process kill).

**Fix (this branch):** in `src/youtube.rs`, resolve the youtubedl-android
classes through the **application** classloader
(`Context.getClassLoader().loadClass(name)`) and call through the resulting
`jclass`, and clear any pending exception if `getInstance` fails during init so
it can't poison a later JNI call. (`load_class` helper.)

After the fix:

- **Playlist resolution works.** yt-dlp `--flat-playlist --dump-json` ran over
  JNI (Python runtime extracted to
  `no_backup/youtubedl-android/packages/python/`, `libpython3.12.so` executing),
  and the grid listed both videos with real titles and decoded YouTube
  thumbnails.
- **Stream resolution works.** Tapping Play showed "Resolving YouTube stream…",
  `yt-dlp -f best[height<=?480] -g` resolved a direct URL over JNI, mpv opened
  it, and audio played (`AHal::Stream`, ~36 s observed). No crash; the app
  returns to the grid gracefully.

### Remaining issue (new, separate from the two caveats): YouTube video is black

YouTube playback produces **audio but a black video frame**, with the scrubber
stuck at `0:00 / 0:00`, then ends early. libmpv emits `libsigchain` signal
backtraces from its decoder threads during this. The local H.264 clip renders
perfectly through the *same* GL/eframe path, so this is not the rendering
pipeline — most likely `best[height<=?480]` resolves to a VP9/webm stream the
vendored ffmpeg can't decode on this device (or a googlevideo/DASH/headers
issue). Suggested follow-up: constrain the format selector to an H.264/mp4
progressive stream (e.g. `best[ext=mp4][height<=?480]` / prefer `avc1`) and/or
raise libmpv's log level to capture the decoder error, then re-test. Not fixed
here because it's outside the two flagged caveats and the fix needs its own
verification.

## Net status of the README "What works / caveats"

- "on-device video rendering has not been run on hardware yet" → **verified.**
- "the JNI execution path is unverified on hardware" → **verified, and the
  predicted classloader crash fixed.** YouTube playlist + stream resolution now
  function on device.
- New follow-up logged: YouTube video decode (black frame) — see above.

## Code change

- `crates/shepherd-media-android/src/youtube.rs`: app-classloader resolution for
  the youtubedl-android JNI bridge (`load_class`) + pending-exception clearing.
  Host tests (27) pass; `cargo fmt` + android-target clippy clean.
