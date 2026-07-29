# shepherd-media performance investigation (issue #115)

## The prompt

> Investigate #115. You may use
> <https://www.youtube.com/playlist?list=PLaE72AVrN19mC3yLknYoE8waTPMnhDWmN> as
> your media library. This *is* the Linux client in question that seems to
> consume high CPU while playing. When checking the phone portion, you may use
> the connected phone (whose passcode is the first 6 digits of Pi, in case it
> locks)

## The issue

[#115 — Investigate shepherd-media performance](https://git.armeafamily.com/albert/shepherd-launcher/issues/115):

> On a Surface Pro, while I have no complains with framerate, it's clearly using
> a lot of CPU and I barely get an hour of playback on its (admittedly old)
> battery.
> On a Fire Stick, I'm getting closer to 20fps on some videos.
>
> There's potentially some nontrivial overhead in how we're playing the video
> here.

## Verdict

The hypothesis in the issue — "nontrivial overhead in how we're playing the
video" — is right, but **the overhead is not in the egui + off-screen-FBO
compositing**. That costs about **1 percentage point of one core**, and the
window presents exactly one frame per decoded frame. The cost is that
**hardware decoding never engages on either platform**, so every frame is
decoded in software.

Three independent causes, two of them app bugs:

| # | Cause | Platform |
|---|---|---|
| 1 | `bind_gl` never passes `MPV_RENDER_PARAM_WL_DISPLAY`, so mpv cannot open a VA display and silently drops to `vaapi-copy` (GPU→RAM readback per frame) | Linux |
| 2 | No VA-API driver package is installed or declared in `scripts/deps/run.pkgs`, so there is no hardware decoder at all | Linux |
| 3 | `Quality::ytdl_format()` has no codec preference, so YouTube serves VP9/AV1 that the GPU cannot decode | Linux |
| 4 | No MediaCodec session is ever created; decoding happens in-process in software | Android |

## Test rig

The machine the issue is about: Surface Pro 1 (2012) — Intel i5-3317U (2c/4t,
1.7 GHz), HD Graphics 4000 (Ivy Bridge, gen7), 1920×1080 internal panel,
Ubuntu 26.04, mpv 0.41, Mesa 26.0.

`dev headless` is **not** usable for performance work: `scripts/lib/headless.sh`
sets `LIBGL_ALWAYS_SOFTWARE=1` unconditionally, so even `--gpu` leaves clients
on llvmpipe. Instead a real GPU session was booted on a spare VT over SSH:

```sh
sudo mkdir -p /run/shepherd-perf && sudo chmod 700 /run/shepherd-perf
sudo setsid openvt -c 3 -s -- env XDG_RUNTIME_DIR=/run/shepherd-perf \
    LIBSEAT_BACKEND=builtin XDG_SESSION_TYPE=wayland sway -c minimal-sway.conf
# clients: sudo env XDG_RUNTIME_DIR=/run/shepherd-perf WAYLAND_DISPLAY=wayland-1 …
# teardown: pkill sway; sudo chvt 1
```

`LIBSEAT_BACKEND=builtin` + `openvt` is what lets root take DRM master without a
login session. CPU was sampled from `/proc/<pid>/stat` over 20 s of steady-state
playback, 20 s in. All figures are **percent of one core**.

Test clips: formats 137 (H.264) and 248 (VP9), both 1920×1080 30 fps, from
`Yi8ShLDlquk` in the playlist above — same video, two codecs.

## Measurements (Linux, 1080p30, fullscreen, real GPU)

| what | CPU |
|---|---|
| bare `mpv --vo=gpu --hwdec=no`, H.264 | 43.8 % |
| bare `mpv --vo=gpu --hwdec=auto-safe`, H.264 (VA-API works here) | 12.0 % |
| bare `mpv --vo=gpu --hwdec=auto-safe`, VP9 (no VP9 block on gen7) | 44.7 % |
| **shepherd-media as shipped, no VA driver — the Surface today** | **76.2 %** |
| shepherd-media as shipped, VA driver installed → `vaapi-copy` | 24.3 % |
| **shepherd-media + WL-display patch + VA driver → `vaapi`** | **12.9 %** |
| shepherd-media + patch + driver, VP9 (still software) | 77.5 % |
| shepherd-media + patch + driver, real YouTube URL from the playlist | 60.9 % |

sway's own compositing was 1.8–3.0 % throughout.

Decode-only cost, 33.4 s of the same 1080p30 clip (`ffmpeg -benchmark`):

| | CPU-seconds |
|---|---|
| H.264 software | 15.3 s |
| H.264 VA-API | 0.42 s |
| VP9 software | 13.3 s |
| VP9 VA-API (falls back to software) | 13.3 s |

## Finding 1 — mpv never gets a display handle, so VA-API can't initialize

`LibmpvPlayer::bind_gl` created the render context with only `ApiType` and
`InitParams`. mpv's VA-API interop then has nothing to open a VA display with —
its `x11` and `wayland` probes have no handle, and its `drm` probe wants a DRM
fd that only the `vo_drm`/GBM contexts supply. From the app's own mpv log
(`SHEPHERD_MPV_LOG`):

```
[libmpv_render/vaapi] Using EGL dmabuf interop via GL_EXT_EGL_image_storage
[libmpv_render/vaapi] Trying to open a x11 VA display...
[libmpv_render/vaapi] Trying to open a wayland VA display...
[libmpv_render/vaapi] Trying to open a drm VA display...
[libmpv_render/vaapi] Could not create a VA display.
...
[vd] Using hardware decoding (vaapi-copy).
```

`vaapi-copy` works because FFmpeg opens `/dev/dri/renderD128` itself, but it
reads every decoded frame back from GPU memory into system RAM and the GL
renderer re-uploads it. `perf record` on the app during H.264 playback put
**37 % of all samples in `__memmove_sse2_unaligned_erms`**, split across mpv's
`core` thread and the render thread — that is the readback.

Passing the `wl_display` fixes it:

```
[libmpv_render/vaapi] Trying to open a wayland VA display...
[vd] Using hardware decoding (vaapi).
```

24.3 % → **12.9 %**, which is within a point of bare mpv (12.0 %). A prototype
patch doing this is in the working tree: a `NativeDisplay` enum in
`shepherd-media-core`, an extra `bind_gl` parameter, and
`eframe::CreationContext::display_handle()` in the Linux binary. Android passes
`None` (its interop is MediaCodec-based and needs no handle).

## Finding 2 — no VA-API driver is installed, and none is declared

`scripts/deps/run.pkgs` lists `mpv`, and Ubuntu's `mpv` package doesn't even
Recommend a VA driver. On this machine libva found nothing:

```
libva: Trying to open …/iHD_drv_video.so   → va_openDriver() returns -1
libva: Trying to open …/i965_drv_video.so  → va_openDriver() returns -1
[vaapi] Failed to initialize VAAPI: unknown libva error
```

`sudo apt install i965-va-driver` (the legacy gen4–gen8 driver; `iHD` only
covers Broadwell and newer, and Mesa ships no VA driver for `crocus`) made
H.264 hardware-decode. **This was installed on the Surface during the
investigation and left in place.**

Gen7's fixed-function decoder covers H.264, MPEG-2, VC-1 and JPEG — not VP9,
HEVC or AV1.

## Finding 3 — the format selector ignores what the device can decode

`Quality::ytdl_format()` is `bestvideo[height<=?1080]+bestaudio/…`. yt-dlp
ranks `av01 > vp9 > avc1` for equal resolution, so on the real playlist mpv's
`ytdl_hook` picked VP9:

```
[vd] Opening decoder vp9 … Using software decoding.
```

60.9 % of a core, against 12.9 % for the *same video's* H.264 rendition.

The Android build already solved this — `shepherd_media_android::youtube::stream_format`
uses `bv*[vcodec^=avc1][height<=?720]+ba/…` with the comment "YouTube's best
DASH video is usually VP9/AV1, which fails to decode on many mobile GPUs". The
Linux selector never got the same treatment.

## Finding 4 — the compositing architecture is fine

Two checks, both negative for the "compositing is expensive" theory:

- With hwdec working, shepherd-media costs 12.9 % versus bare mpv's 12.0 %. The
  extra full-screen FBO plus egui's textured quad is worth about one point.
- `WAYLAND_DEBUG=1` frame accounting during 30 fps playback: **358
  `wl_surface.commit`s in 11.9 s = 30.2 frames/s**. The `request_repaint_after`
  cadence in `ui/playback.rs` is not causing redundant renders.

The FBO round trip *is* visible when the pipeline is already
memory-bandwidth-bound: with software decode the app costs 76 % against bare
mpv's 44 %. That gap closes on its own once decoding moves to the GPU.

## Finding 5 (Android) — MediaCodec is never used

Verified on the connected Pixel 10a (Android 17) playing a 720p H.264 YouTube
stream through the installed `com.armeafamily.shepherd.media` 0.1.0:

- app process: **46–57 % of one core**, sustained;
- `dumpsys media.metrics`: **zero** codec sessions attributed to the app;
- `media.swcodec`: 0.0 % — so no Codec2 software codec either.

Decoding is happening inside the app process, in FFmpeg's software decoder. On
a Pixel big core that is merely wasteful; on a Fire TV Stick's Cortex-A53 it is
exactly the reported ~20 fps.

The capability is present but unreachable:

- `vendor/libmpv/*/libavcodec.so` ships `h264_mediacodec`, `hevc_mediacodec`,
  `vp9_mediacodec`, `av1_mediacodec` and links `libmediandk.so`;
  `libmpv.so` has `mediacodec`, `mediacodec-copy` and `vo_mediacodec_embed`.
- Per the mpv manual, `--hwdec=mediacodec` (zero-copy) **requires**
  `--vo=gpu --gpu-context=android` or `--vo=mediacodec_embed`. The app uses the
  libmpv GL render API, so only `mediacodec-copy` is reachable.
- `mediacodec-copy` still fails, and the likely reason is that
  **`av_jni_set_java_vm()` is never called**: nothing in the workspace calls it,
  `libmpv.so` exports no `JNI_OnLoad`, and the 2026-06-28 on-device notes
  already recorded libmpv logging `ao/audiotrack: No Java virtual machine has
  been registered`. FFmpeg's MediaCodec decoder resolves a codec name through
  `MediaCodecList`, which is JNI-only — no JVM, no codec. *(This last step is
  inferred from the evidence above rather than proven by a rebuild.)*

## What was implemented

On branch `fix/media-hardware-decoding`.

1. **Pass the display handle to `mpv_render_context_create`.** A `NativeDisplay`
   enum in `shepherd-media-core`, an extra `bind_gl` parameter, and
   `eframe::CreationContext::display_handle()` in the Linux binary. Android
   passes `None` — its interop is MediaCodec-based and needs no handle.
2. **Report the decode path.** `LibmpvPlayer` observes mpv's `hwdec-current` and
   logs it per file: `info` for a zero-copy interop, `warn` for software or for a
   `-copy` mode. The workspace `tracing` gained the `log` feature so the Android
   binary's `android_logger` sees these too. This is the diagnostic whose absence
   made the whole bug invisible.
3. **Prefer H.264 in `Quality::ytdl_format()`**, with any-codec and muxed
   fallbacks, mirroring the Android `stream_format` decision.
4. **Install VA-API drivers**, as two new `shepherd-admin` targets alongside the
   existing `yt-dlp` one:

   - `va-api install` picks driver packages from the GPUs found under
     `/sys/bus/pci/devices` (falling back to the DRM driver name where there is
     no PCI display controller, as on ARM boards), filtered to what the host's
     archive actually offers. `va-api detect` prints the same analysis plus
     which driver `.so` is already present, without installing anything.
   - `media-deps install` runs `va-api` and `yt-dlp` together — the two things
     shepherd-media needs that apt cannot cover on its own — and is what
     `shepherd deps install run` now calls.

   None of this is in `run.pkgs`: that file is one unconditional apt
   transaction, and the right driver depends on the hardware. Documented in
   `docs/INSTALL.md` and `docs/shepherd-media.md`.

   For Intel the resolver deliberately offers *both* `intel-media-va-driver`
   (iHD, Broadwell and newer) and `i965-va-driver` (older), because nothing in
   sysfs cleanly separates the two generations and libva already probes them in
   turn. Verified on the HD 4000 with both installed: iHD's `vaInitialize`
   fails, libva falls back to i965, and playback still reports
   `vaapi (zero-copy)` at 13.9 % of a core.

   The detection is exercised against fixture sysfs trees via
   `SHEPHERD_SYSFS_ROOT`, which is how the AMD / nouveau / proprietary-NVIDIA /
   ARM / hybrid-graphics branches were checked on Intel-only hardware.
5. **`av_jni_set_java_vm()` on Android startup** (`src/ffmpeg.rs`, called from
   `android_main`), with `libavcodec` added as a link-time dependency in
   `build.rs`.
6. **Invalidate cached videos when the selector changes.** Found while verifying
   (3): the first "fixed" run still played VP9, because `VideoCache` had a
   `.webm` from an earlier run and keys purely on item id. The done sentinel now
   records the selector that produced the file; a mismatch makes the *download*
   path replace it, while `cached_path` keeps serving the old file meanwhile so
   an offline device never loses content it already has.

### Result, measured on the same YouTube playlist

| | shepherd-media CPU |
|---|---|
| before, as the Surface was configured | **76 %** of one core |
| after, streaming fresh | **11.3 %** of one core |

The app log now reads `mpv is decoding video with vaapi (zero-copy)`, and
`mpv --hwdec` picks `h264` rather than `vp9`. Browse → Enter → playback verified
end to end on the real panel.

### Android result

Rebuilt and installed on the Pixel 10a. Before: no MediaCodec session existed
while playing. After: a `c2.exynos.h264.decoder` session at 720p with 1463 frames
decoded, and `registered the JavaVM with FFmpeg` in logcat. Hardware decoding is
reached.

App CPU, however, barely moved: 46–57 % before, 40–47 % after. A pause/resume
comparison explains why — **paused ≈ 40 %, playing ≈ 41 %**. On this device
almost all of the app's CPU is the egui/GL loop, not the video pipeline, so
moving decode to the hardware block frees very little.

## Fire TV: the ~20fps is the render loop, not decode

Measured afterwards on the real device (Fire TV `AFTHA004`, Amlogic quad-core,
armeabi-v7a, Fire OS / Android 9, output forced to 1920×1080 @60 Hz on a 4K
panel), driving the app over network adb. Local clips in an `.m3u` under the
app's own data dir, settings written directly via `run-as`. Presented fps comes
from `dumpsys SurfaceFlinger --latency`, unioned across polls because the ring
buffer only holds 128 frames.

Two builds: the branch as committed, and a *baseline* identical except that
`ffmpeg::register_java_vm()` is not called.

| clip | baseline | with JVM registration |
|---|---|---|
| 720p H.264 30fps | 30.0 fps, 0.49 core | 30.0 fps, 0.57 core |
| 1080p H.264 30fps | 29.9 fps, 0.69 core | 30.0 fps, 0.70 core |
| 720p VP9 30fps | 30.0 fps, 0.50 core | 30.0 fps, 0.59 core |
| 1080p VP9 30fps | 30.0 fps, 0.65 core | 30.0 fps, 0.70 core |
| **1080p H.264 60fps** | **19.9 fps, 0.74 core** | **22.8 fps, 0.83 core** |

**The reported symptom reproduces on 60fps content and only there.** Every
30fps clip holds a solid 30.0 fps at either resolution, in either codec. The
60fps clip presents at ~20–23 fps with a median frame interval of 50 ms —
exactly one frame per three vsyncs.

**Two things this rules out.**

*Decode is not the bottleneck, and the JVM fix does not help here.* Both builds
instantiate `OMX.amlogic.avc.decoder.awesome` at 1920×1080, attributed to the
app's own pid in logcat. FFmpeg's MediaCodec wrapper has an NDK backend
(`libavcodec` links `libmediandk.so`) that needs no `JavaVM`, so this device was
already decoding in hardware before the change. The registration mattered on the
Pixel (Android 17) — and it is still needed for the `audiotrack` AO — but it is
**not** the Fire TV fix. An earlier draft of this note asserted it would be;
that was wrong.

*It is not CPU exhaustion.* The app never exceeds ~0.83 of one core out of four,
and its busiest single thread sits at 35–44 %. Re-running under four competing
busy loops — the device pinned at 100 % of all four cores, roughly emulating
much weaker silicon — barely moves it: 1080p30 goes 30.0 → 27.9 fps, and
1080p60 does not degrade at all (22.8 → 24.4 fps, i.e. noise).

What is left is the compositing/present path. This is the same wall the existing
comment in `shepherd-media-android/src/playback.rs` describes ("left the Fire TV
presenting ~15 fps even though decode kept up") — and the unconditional
`ctx.request_repaint()` added to work around it does not raise the ceiling, it
just spends the budget differently.

**The Fire TV half of issue #115 was not fixed by the hardware-decoding
commit** — it needed the SurfaceView change recorded below.

### Which part of the compositing is expensive

The off-screen target is always sized to the *screen*, so the FBO write and the
FBO→back-buffer blit cost the same no matter what the video's resolution is.
Playing a 60fps clip at two source resolutions therefore separates the fixed
full-screen cost from the per-source-pixel cost:

| clip | presented | app CPU | CPU per presented frame |
|---|---|---|---|
| 1080p60 | 22.8 fps | 0.83 core | 36.4 ms-core |
| 720p60 | 45.8 fps | 1.06 core | 23.1 ms-core |
| 1080p30 | 30.0 fps | 0.70 core | 23.3 ms-core |
| 720p30 | 30.0 fps | 0.57 core | 19.0 ms-core |

**Throughput doubled when the source halved, while the fixed full-screen work
was unchanged.** So the dominant term scales with source pixels per second —
the per-frame copy-out, upload and scale — not the full-screen FBO round trip.
Removing the FBO would be a second-order win, not the fix.

The per-presented-frame column shows the second effect: at 1080p60 each
presented frame costs 36.4 ms-core against 23.3 at 1080p30, because the app is
handling 60 source frames a second and showing 22.8 of them. Roughly a third of
the work is spent on frames that never reach the screen. At 720p60, where 46 of
60 are shown, that overhead shrinks to 22 %.

And it is still not CPU-bound even at 720p60: 1.06 of four cores, busiest
thread 53 %. Neither the device nor any single thread is saturated, which points
at the GPU and the CPU→GPU transfer as the constraint — i.e. exactly the
per-frame copy that `mediacodec-copy` forces.

### Fixed: video now decodes into a SurfaceView

Implemented as lever 1 below. mpv decodes into an `android.view.Surface`
(`vo=mediacodec_embed`, `hwdec=mediacodec`, `wid`) owned by a `SurfaceView`
behind a translucent window, and the GL side paints only the transport overlay.
Same clips, same device:

| clip | before | after |
|---|---|---|
| 720p H.264 30fps | 30.0 fps, 0.57 core | 30.0 fps, 0.20 core |
| 1080p H.264 30fps | 30.0 fps, 0.70 core | 30.0 fps, 0.26 core |
| 720p VP9 30fps | 30.0 fps, 0.59 core | 30.0 fps, 0.23 core |
| 1080p VP9 30fps | 30.0 fps, 0.70 core | 30.0 fps, 0.25 core |
| **1080p60 H.264** | **22.8 fps, 0.83 core** | **48.2 fps, 0.48 core** |
| 720p60 H.264 | 45.8 fps, 1.06 core | 47.5 fps, 0.37 core |

The 60fps ceiling is gone — median frame interval 16.7 ms where it was 50 ms —
and jitter improves everywhere: the worst interval on 30fps content falls from
83–116 ms to 33.4 ms. The UI layer now posts ~8 frames/s instead of one per
display refresh.

One more trap, this one a crash rather than a black screen: **do not hand mpv
`wid = -1` to detach.** `stop()` only queues the teardown, so if the VO thread
reconfigures before it finishes it re-enters `create_mediacodec_device_ref`,
finds `WinID == -1` and aborts the process. It presented as playback crashing
roughly one time in four when leaving with the back button — random-looking,
because it is a race. Nothing needs detaching: the Surface outlives any single
playback and the next `acquire` replaces the reference. Verified with 20
consecutive play→back cycles (it used to die within two), plus backgrounding
mid-playback and playing to EOF.

Two more things about verifying this, both of which look like a black screen:

- **`adb screencap` returns black for a hardware overlay plane.** It can no
  longer confirm playback. Read the SurfaceView layer's rate from
  `dumpsys SurfaceFlinger --latency 'SurfaceView - <pkg>/<activity>#0'`
  instead — and note the video and the UI are now *different* layers, so a
  sampler pointed at the window layer reads zero while playback is perfect.
- **A wedged device looks identical to a broken build.** Twice during this work
  the Fire TV got into a state where every build — including known-good ones —
  spun at ~100 % of a core with no frames presented, and only a reboot cleared
  it. A first pass at this change was wrongly diagnosed as a structural blocker
  on that basis. Reboot the device and re-test before concluding anything from
  a black screen, and check `dumpsys display | grep mScreenState` while you are
  at it.

### Ranked ways to cut it

1. **Zero-copy decode on Android.** *Done — see above.* `mediacodec-copy` read
   every decoded frame back into system RAM for mpv to re-upload as textures;
   that was the term the measurements said dominates, and removing it cut CPU
   by ~3x and lifted the 60fps ceiling.
2. **Stop rendering frames that are never presented** — worth about a third of
   the work at 1080p60. Two parts: honour the `needs_render` flag both
   front-ends maintain and then discard (`let _new_frame = …`), and drive the
   render API the way it is meant to be driven, with
   `MPV_RENDER_PARAM_ADVANCED_CONTROL` plus `NEXT_FRAME_INFO` /
   `BLOCK_FOR_TARGET_TIME` (all exposed by libmpv2 as `RenderParam::`
   `AdvancedControl`, `NextFrameInfo`, `BlockForTargetTime`, `SkipRendering`) so
   mpv says when the next frame is due instead of the host guessing. Needs care:
   the unconditional repaint exists precisely because callback-driven rendering
   regressed to ~15 fps.
3. **Size the off-screen target to the video, not the screen.** `target_size` is
   screen pixels today, so 720p content is upscaled by mpv into a 1080p FBO and
   then drawn by egui at 1:1. Sizing the FBO to the video removes mpv's scale
   pass and shrinks its write; egui's existing textured quad does the upscale
   with the same bilinear filter `profile=fast` already selects. Cheap, low
   risk, and it targets exactly what the Android YouTube path produces, since
   `stream_format` caps at 720p.
4. **Remove the FBO round trip** — draw mpv into the default framebuffer from an
   `egui::PaintCallback` and let egui paint only the overlay. Saves one
   full-screen write and one full-screen read per frame. Second-order per the
   table above.

### The same split on Linux, in watts

The Surface's complaint was battery, not framerate, so the Linux side is better
measured with Intel RAPL than with CPU percentages. `package-0` is the whole
SoC, `core` the CPU cores, `uncore` the integrated GPU. Fullscreen 1080p on the
1920×1080 panel, zero-copy VA-API, 20-second samples, conditions interleaved to
cancel thermal drift:

| condition | package | core | uncore (iGPU) |
|---|---|---|---|
| sway idle, black screen | 2.80 W | 0.71 W | 0.00 W |
| shepherd-media, 1080p source | 8.28 / 8.66 W | 1.86 / 2.09 W | 3.39 / 3.49 W |
| shepherd-media, 720p source | 8.04 / 8.10 W | 1.74 / 1.76 W | 3.37 / 3.40 W |
| bare `mpv --vo=gpu --hwdec=auto-safe` | 9.24 / 9.41 W | 2.21 / 2.35 W | 4.27 / 4.28 W |
| shepherd-media, paused | 3.23 W | 0.81 W | 0.18 W |

Three things fall out, and the first two invert the Fire TV's conclusions.

**The GPU cost does not scale with source resolution — 3.4 W either way.** The
opposite of the Fire TV, and for a clear reason: Linux decodes zero-copy, so the
decoded surface is imported as a texture via dmabuf and there is no per-frame
upload proportional to source pixels. What remains is two full-screen passes —
mpv into the FBO, egui's quad into the back buffer — whose cost is set by the
*output* resolution. Fixing hardware decoding is what moved Linux from
source-scaled to fixed-cost.

**The compositing is not a regression against a plain player.** Bare mpv draws
~0.9 W *more* than shepherd-media at an identical 30.1 surface commits per
second (both verified with `WAYLAND_DEBUG`), despite doing one full-screen pass
where the app does two. So whatever the extra FBO round trip costs, it is
smaller than the difference between mpv's own output path and the app's — which
caps how much removing it can be worth here.

**Nothing is wasted while paused.** 3.23 W against 2.80 W idle: after the
overlay's 3-second window the repaint drops to the 250 ms tick and the GPU goes
quiet.

### Which lever helps which platform

| lever | Fire TV | Linux |
|---|---|---|
| 1. zero-copy decode | the dominant cost | **already done** — and it is why Linux is now fixed-cost rather than source-scaled |
| 2. skip frames never presented | ~⅓ of the work at 1080p60 | little — already presents 30.1/s, same as bare mpv, and idles correctly when paused |
| 3. FBO sized to the video | helps (source-scaled) | ~nothing — uncore is identical for a 720p and a 1080p source |
| 4. remove the FBO round trip | second-order | the only lever aimed at Linux's 3.4 W, but the bare-mpv comparison says its marginal cost is small |

So the two platforms want opposite work, and the Linux build has already had its
big win: 76 % → 11.3 % of a core, with playback now costing ~5.6 W over idle of
which ~3.4 W is the iGPU.

### The two platforms are not symmetric — mpv has no Wayland embedding

An earlier revision of this note proposed the same fix for both platforms: give
the video its own surface so the compositor can hardware-compose it. That works
on Android and **does not work on Linux**, because of what libmpv actually
offers.

`--wid` attaches a VO to an existing window on **X11, win32 and Android only**.
There is no Wayland equivalent — no `--wayland-*` embedding option exists. So on
Wayland the libmpv **render API is the only embedding path**, it is OpenGL, and
therefore:

- every frame costs at least one full-screen GL pass, and
- `--vo=dmabuf-wayland` — the driver that avoids GPU↔CPU copies and does scaling
  and colour conversion on fixed-function hardware, which is what would make
  presentation nearly free — is unreachable, because it needs mpv to own its own
  Wayland window.

A `wl_subsurface` of our own is still possible (bind `wl_subcompositor` on the
`wl_display` that `RawDisplayHandle::Wayland` already hands us, attach a
`wl_egl_window`, build mpv's render context against that GL context). But it
only removes the egui blit — one of the two passes — which an
`egui::PaintCallback` removes far more cheaply.

### Direct scan-out is not available here, twice over

The one remaining argument for the subsurface was that unmapping the overlay
while the controls are hidden would leave the video as the sole fullscreen
surface, letting wlroots direct-scan-out it. That argument fails for two
independent reasons, both checked.

**The HUD is always mapped during a session.** `shepherd-hud` is a GTK4
layer-shell surface on `Layer::Overlay`, anchored left/right across the output
with a non-zero exclusive zone, and `set_visible` follows
`session_state.is_visible()` — so it is up exactly when media is playing. wlroots
scans out only a single surface covering the output, so the video is never a
candidate.

**And scan-out already fails on this hardware regardless.** Running sway with
`-d` and a fullscreen `shepherd-media`, its log fills with

```
[wlr] [backend/drm/drm.c:769] connector LVDS-1: Failed to import buffer for scan-out
```

581 times with nothing else mapped, 1021 times with a layer-shell bar added —
the same message either way. wlroots attempts scan-out and the display
controller rejects the buffer, so occlusion was never the binding constraint;
the eframe/glow render buffer simply is not importable for scan-out on this
i915 connector.

Consistent with that, adding a mapped layer surface costs nothing measurable:
uncore 3.33 W both with and without swaybar present, package 8.31 W → 8.49 W,
and that 0.18 W is the bar process and its status command. sway composites
either way, so there was never any scan-out to lose.

This also undercuts `vo=dmabuf-wayland` on *this* machine: its advantage is
handing the compositor a dmabuf of the decoded frame for fixed-function scaling
and colour conversion, which pays off when the result can be scanned out or
placed on a plane. If the display controller will not import our buffers, the
compositor has to composite it on the GPU anyway.

So the Linux ordering is: try the cheap paint-callback form of lever 4 and
measure it. Beyond that, ~3.4 W of iGPU may simply be near the floor for
GL-composited 1080p video on an HD 4000, and the remaining battery life is a
property of the machine rather than of this code.

### Also found: a display-size change wedges the app

Changing the display size with `wm size` while the app was installed left it
rendering black with two threads spinning at 100 %, and it stayed wedged after
`am force-stop`, after restoring the original 1920×1080 override, and across
relaunches. Only a device reboot recovered it. Not characterised further — in
particular it is not established whether the trigger is a non-native surface
size or the resize itself — but a TV whose output resolution changes is a
plausible real-world path into it, so it deserves its own issue.

## Follow-ups worth their own issues

- **The Android compositing path is the Fire TV bottleneck** (measurements
  above). Per presented frame it runs an mpv render pass into an off-screen FBO,
  a full egui frame, and a blit of that FBO to the back buffer, all at the
  output resolution. The device sustains 20–30 of those per second, so 60fps
  content presents at ~20. Two threads to pull:
  - `playback.rs` calls `ctx.request_repaint()` unconditionally while playing
    and ignores the `needs_render` flag it maintains (`let _new_frame = …`), so
    it re-renders mpv's current frame whether or not a new one exists. The
    Linux binary instead relies on mpv's update callback and presents exactly
    30.2 fps for 30 fps content. Note the unconditional repaint was deliberate —
    the comment records the callback cadence leaving a Fire TV at ~15 fps — so
    this is not a straight revert.
  - The FBO round trip exists so egui can composite the video as a texture.
    Painting the video directly and drawing only the overlay through egui would
    remove one full-screen read and write per frame.
- **Zero-copy on Android TVs.** `mediacodec-copy` still round-trips every frame
  through system RAM. The zero-copy `mediacodec` hwdec needs an Android
  `Surface` and `vo=mediacodec_embed` (or `vo=gpu --gpu-context=android`), which
  means a SurfaceView playback screen instead of compositing into the egui
  surface — a real design change.
- **Codec preference could be derived rather than hardcoded.** H.264-first is
  right for the hardware this project targets, but a modern GPU with AV1 decode
  would get better quality per byte from the newer codecs.

## Loose ends

- `shepherd-media` dies with SIGABRT (core dump) when it receives SIGTERM during
  playback. Seen on every teardown in this investigation; not chased down.
- The playlist item id for a YouTube library is the **lowercased** video id
  (`sanitize_video_id`), e.g. `--item yi8shldlquk`, not `Yi8ShLDlquk`.
- `i965-va-driver` was installed on the Surface during this work and left in
  place; it is what `deps install run` would now install anyway.
- The Pixel now carries a locally-built debug-signed APK of this branch.
