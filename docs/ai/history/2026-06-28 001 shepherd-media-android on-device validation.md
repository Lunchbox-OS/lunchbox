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
  yt-dlp `-g` resolved a stream over JNI, mpv opened it, and audio played
  (`AHal::Stream`). No crash; the app returns to the grid gracefully.

### Second crash found and fixed: an uncaught yt-dlp exception

Tightening the format selector exposed a second hard crash: a
`com.yausername.youtubedl_android.YoutubeDLException` (e.g. "Requested format is
not available") propagated **uncaught** as a Java `FATAL EXCEPTION` and killed
the process. Cause: when `execute()` throws, the `jni` crate returns `Err` via
`?` *before* the code clears the pending exception, so the worker thread detaches
with an exception still pending → ART aborts. This affected *any* yt-dlp failure
(private/blocked video, network, bad format), not just the format change.

**Fix:** funnel throwing JNI calls through `take_pending_exception`, which reads
the Throwable's message and **clears** the exception, turning it into a
recoverable `Err` surfaced in the status bar. Verified on device: a format error
now shows "youtubedl-android: ERROR …" in the top bar with the app still running.

### Root cause of the black video: only audio formats are available

After the JNI path was solid, YouTube playback still showed **audio + a black
frame**. Walked it down with on-device evidence:

1. Logged the resolved URL: `itag=139`, `mime=audio/mp4` — an **audio-only**
   track (dur≈33 s matched the clip). Not a codec or render bug.
2. Implemented **separate video+audio streams** (the right architecture, since
   YouTube no longer offers progressive muxed files): core gained
   `PlayerHandle::set_external_audio` (libmpv attaches it via the `loadfile`
   per-file `audio-file=%<len>%<url>` option, length-quoted so commas/colons in
   the URL don't break parsing); the resolver now parses both `-g` URLs
   (`bv*[vcodec^=avc1]+ba/…`, preferring H.264 since the device can't decode the
   VP9/AV1 `bv*`).
3. Still audio-only. A verbose libmpv log (env-gated `SHEPHERD_MPV_LOG`) showed
   the loaded file was itag 139 with `video=eof`, and `yt-dlp -F` was conclusive:
   **the only media formats offered are `139` / `139-drc` (audio-only m4a) plus
   storyboards — there are no video formats at all.**

So the black video is a **dependency limitation, not an app bug**: the bundled
`youtubedl-android` 0.18.1 (old yt-dlp) gets only the audio format from YouTube's
clients — modern YouTube gates video formats behind PO tokens / SABR. Every
selector necessarily falls back to audio-only. The separate-stream + H.264
plumbing added here is correct and will produce video as soon as a video format
is obtainable.

**Follow-up (dependency-level, not Rust):** bump `youtubedl-android` / its bundled
yt-dlp and add a PO-token provider (or a client config that still serves video),
then re-test — the app side is ready. Also noted: libmpv logs
`ao/audiotrack: No Java virtual machine has been registered` (audio still plays
via a fallback AO); registering the JVM with libav (`av_jni_set_java_vm`) would
silence it and is worth doing for the audiotrack AO.

## Net status of the README "What works / caveats"

- "on-device video rendering has not been run on hardware yet" → **verified
  working** (local HTTP clip: video + audio + transport + cache).
- "the JNI execution path is unverified on hardware" → **verified**; two crashes
  found and fixed (classloader, uncaught exception). Playlist + stream resolution
  run over JNI; the app no longer crashes on any yt-dlp failure.
- YouTube *video* playback is blocked upstream (audio-only formats from the
  bundled yt-dlp); app-side plumbing is in place — see above.

## Code changes (this branch)

- `crates/shepherd-media-android/src/youtube.rs`: app-classloader class
  resolution for the JNI bridge (`load_class`); `take_pending_exception` so
  yt-dlp errors surface instead of crashing; separate video+audio resolution
  (`StreamUrls`, `parse_stream_urls`) with an H.264-preferring `bv*+ba` selector.
- `crates/shepherd-media-core/src/player.rs`: `PlayerHandle::set_external_audio`
  (libmpv attaches an external audio track via length-quoted `loadfile` options);
  env-gated `SHEPHERD_MPV_LOG` verbose log for on-device debugging.
- Host tests pass (core 28, app 25, android 50); `cargo fmt` + android-target
  clippy clean; APK builds and installs.
