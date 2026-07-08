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

## Follow-up: text fields trap D-pad focus (same session)

> if the focus ends up in a text box, it is impossible to leave the text box —
> the arrows don't exit it, and the back button goes to the previous screen
> entirely

A focused egui `TextEdit` sets a focus lock filter with
`horizontal_arrows: true, vertical_arrows: true` (arrows move the text cursor,
not focus). On a remote — which has no other way to move focus — that traps the
add-library form's Id / Label / Location fields. The user's only escape was BACK,
which the screen-level handler treated as "navigate up a screen".

Fixes (`src/ui.rs`, add-library form is the only screen with text fields):

- **Free the vertical arrows.** After each `TextEdit`, override its lock filter
  (`tv_free_field_focus`) to `vertical_arrows: false` so Up/Down move focus
  between the stacked fields and off to the buttons; keep `horizontal_arrows:
  true` so Left/Right still move the cursor (useful in the desktop preview,
  harmless on a remote where text is entered via the phone hand-off / file
  browser, not typed). Overriding *after* the widget runs wins for that frame's
  end-of-frame focus move.
- **BACK leaves the field, not the screen.** Track whether a field held focus
  this frame (`text_field_focused`); if so, BACK calls `Memory::stop_text_input()`
  (clears focus) instead of navigating up. Focus falls back to the screen's first
  control on the next frame; a second BACK then leaves the screen as before.

Verified on the AFTHA004: from a focused Id field, Down moves to Label (no longer
trapped); BACK releases the field and stays on the Add-library screen.

## Follow-up: add-library form D-pad navigation (same session)

> yes, fix the field navigation too

The aside above became the ask. egui's spatial focus walks the vertically-aligned
left-column buttons (Back / Browse / Add) and skips the offset field column, so a
remote's Down never reaches the fields — you had to press Right. Fixed by driving
focus explicitly in `add_library_screen` (`src/ui.rs`):

- **Tab-order Up/Down.** Capture every control's `Response` (Back, Id, Label,
  Source, Location, the one Browse/Phone action button, Add) into an ordered
  list; on Up/Down, find the focused one and `request_focus()` the previous/next
  (wrapping), then `move_focus(None)` to cancel egui's spatial move. Down now
  steps Back → Id → Label → Source → Location → Browse → Add. Verified on device.

Testing that surfaced two more remote gaps in the **Source `ComboBox`** — egui
only closes a combo popup on a *pointer click* or *Escape* (see
`popup.rs`: `close_click = … pointer.any_click()`), and Android sends BACK as
`BrowserBack`, not Escape:

- **Enter didn't close the popup.** A D-pad Enter fires the `selectable_value`
  (the kind changes) but leaves the popup open. Detect the change (`prev_kind`)
  and `Popup::close_all`, then re-focus the combo so focus doesn't drop to the
  first widget.
- **BACK skipped past the open popup and left the screen.** Snapshot
  `Popup::is_any_open` *before* the screen renders (egui may close it on Escape
  mid-render), and in the BACK handler dismiss an open popup first — ahead of the
  text-field and screen-navigation cases — so BACK closes the dropdown instead of
  navigating. While the popup is open, the form's Up/Down stepping is skipped so
  egui drives the options.

All verified on the AFTHA004: Down reaches every field; the Source dropdown opens,
navigates, selects (closing the popup, focus kept on Source), and BACK dismisses
it without leaving the screen. Back-handler order is now popup → text field →
screen; regression-checked that field-BACK and plain-BACK still behave.

## Follow-up: same fixes in the library editor, via shared helpers (same session)

> I need these fixes in the library *editor* too, not just the add library page.
> Ideally these should be pulled out into their own helpers rather than being
> copy/pasted.

The editor is the Settings screen's per-library `caching_editors` (three
`ComboBox`es — Cache / Quality / Posters — plus a Limit drag value). On device it
had the same faults: Down jumped `active` → `Remove`, skipping the combos, and the
combos didn't close on a D-pad Enter. Pulled the add-form logic into shared
helpers (`src/ui.rs`) and applied them to both screens:

- `tv_combo(ui, id_salt, &mut current, &[(value, label)])` — a D-pad-friendly
  `ComboBox`: shows the options, and on a pick closes the popup (`Popup::close_all`)
  and re-focuses the combo. Replaces all four inline combos (source kind + the
  three caching combos); the `cache_mode_label`/`poster_label` helpers became dead
  and were removed.
- `tv_focus_step(ui, &[&Response])` — the explicit Up/Down tab-order stepping,
  extracted verbatim. The caller builds the ordered response list (unavoidably
  imperative in egui); the helper does the wrap-around focus move and the
  popup-open skip. `caching_editors` now returns its three combo responses so
  `settings_screen` can thread them, the `active` toggle, and the move/remove
  buttons into one order. The Limit drag value is intentionally left out — it owns
  arrow-key handling and is reachable via Left/Right from Posters.
- `tv_free_field_focus` was already a helper (add-form text fields; the editor has
  no text fields). BACK-dismisses-open-popup stays where it was — once, globally,
  in the update loop — so it already covered the editor.

Verified on the AFTHA004: in the editor, Down now steps Back → Add → active →
Cache → Quality → Posters → Remove (reaching every combo), and each combo opens,
navigates, and selects (popup closes, focus kept). Add-form regression re-checked
after the refactor.

## Follow-up: choppy playback (same session)

> playback is still choppy … See what's going on here

Diagnosed on the AFTHA004 with a YouTube-playlist library, measuring rather than
guessing:

- `top`: during playback the app used ~35 % CPU and the system was >50 % idle —
  **not** compute-bound, so not software decode.
- logcat: `OMX.amlogic.avc.decoder.awesome` at 1280×720 — **hardware** H.264
  decode was already working (the `stream_format` selector prefers avc1 ≤720p).
- `dumpsys SurfaceFlinger --latency` on the NativeActivity layer: the display
  runs at 60 Hz but the app presented a new frame only every ~4th vsync —
  **median 66.7 ms ≈ 15 fps**, very regularly. So the bottleneck was the
  **GL render/present**, not decode.

Root cause: mpv's default GL video renderer (high-quality scaler + dither +
debanding) can't upscale 720p → the 1080p output surface within a frame on the
Fire TV's Amlogic Mali GPU, so it presents at a fraction of the refresh rate.

Fix — two parts, in order of impact:

- **`profile=fast`** on the libmpv init (bilinear scaling, no dither/deband).
  This alone took the measured present rate from ~15 fps → ~60 fps. `LibmpvPlayer::new`
  gained a `fast_render: bool`; the Android app passes `true`, the desktop binary
  `false` (desktop GPUs render the full-quality path fine). Best-effort set so a
  libmpv without the profile still runs.
- **Render at display rate while playing** (`playback.rs`): the mpv render API is
  host-driven, so during playback we now `request_repaint()` every frame instead
  of leaning on mpv's update-callback cadence (which, even after the GPU was
  unblocked, is the correct way to keep presentation at the refresh rate). Paused
  playback still idles at the slow tick.

Verified end-to-end on a signed **release** APK: hardware decode, present rate
59.9 fps (median 16.7 ms) at ~44 % CPU, video smooth on screen.

Note on measurement flakiness: YouTube resolution via yt-dlp is slow (~15 s,
CPU-bound under the bundled Python/quickjs) and the browse grid loads
asynchronously, so scripted "open grid → play" often raced the load; each
present-rate number here was taken only after confirming a decoded frame
(`HW.video.avc Got First Frame Ready`) and video on screen. The slow yt-dlp
resolve is startup latency, separate from the playback-smoothness fix.

## Follow-up: dropdown nav, screensaver, yt-dlp speed (same session)

> the screensaver appears after some time in video, and dropdowns (particularly
> the library quality selector) cannot be navigated
> …
> look into the YouTube resolution speed

Three independent issues:

- **Combo popups un-navigable by remote.** egui doesn't reliably move keyboard
  focus *into* an open combo popup, so a D-pad Down either did nothing or
  spatially escaped to a neighbour (the Limit drag value), and Enter never closed
  it (egui closes a combo only on a *pointer* click). Reworked `tv_combo`: while
  the popup is open it *consumes* Up/Down/Enter and drives the choice itself —
  Up/Down cycle the value in place (the list's highlight follows), Enter commits
  and closes and re-focuses the combo. It recomputes the combo's own id
  (`make_persistent_id(Id::new(salt))`, matching `from_id_salt`) to query
  `ComboBox::is_open`. Verified on the AFTHA004: the Quality selector cycles
  1080p→720p→480p and Enter closes it.

- **Screensaver blanks mid-video.** With `vo=libmpv` there's no player window, so
  nothing inhibits the Fire TV screensaver. Set the `KEEP_SCREEN_ON` window flag
  via `AndroidApp::set_window_flags`.

  *First attempt (broken, reverted):* a per-frame toggle from `App::update`
  (on while `playing`/`playback_pending`). This **deadlocked** — `set_window_flags`
  takes android-activity's activity lock (`native_activity.mutex`), which winit
  already holds on the same thread while dispatching input/redraw to the eframe
  loop, so re-locking it from inside `update` is a re-entrant deadlock → ANR →
  crash, exactly on play (when the flag first flips on). Confirmed via logcat
  (`ANR … the focused window has not finished processing … input events`).

  *Fix:* set the flag **once, from `android_main` before `eframe::run_native`** —
  nothing holds that lock at startup. The screen stays on for the app's whole
  foreground lifetime (fine for a TV media player) and never touches the render
  loop. Verified: `dumpsys window` shows `fl=KEEP_SCREEN_ON …` at launch, and
  playback no longer hangs.

- **Slow YouTube resolve (~15 s).** Profiled: CPU-bound in the bundled
  Python/yt-dlp (extraction + the nsig JS challenge under quickjs), not network.
  youtubedl-android's default `execute` passes `--no-cache-dir`, so every resolve
  re-downloads and re-parses YouTube's player JS. Decompiling the AAR showed an
  `execute(request, processId, useCache)` overload (default `useCache=false`);
  switched our JNI call to pass `true`, which makes yt-dlp cache the extracted
  player in the app cache dir so repeat resolves — and the next launch — skip that
  work. The URL is passed as the process id (unique per call).

Testing note: the browse grid, when idle, doesn't repaint continuously, so
scripted `adb` key/tap events into it were unreliable this session (compounded by
the Fire TV's own screensaver backgrounding the app to Netflix). The dropdown fix
and the yt-dlp call were verified on device (the grid *loads its playlist* through
the changed `execute`, proving the new signature); the screensaver flag couldn't
be observed under sustained scripted playback and is left for confirmation on a
real remote. Shipped in a signed release APK.

## Follow-up: DRM-protected playlist videos wouldn't load (same session)

> when I was testing there was at least one video in that playlist that wouldn't
> load — find it and figure out what's wrong

Reproduced the app's exact resolve (same format selector + `player_client=
android_vr`) against every video in the test playlist with host yt-dlp. Three
failed, all licensed "full episode" uploads (PBS / Disney–Muppets):
`1gbjxQ3evP4`, `B7UmUX68KtE`, `PcoAZGb4h5g`. `android_vr` reports them "not
available"; the `tv` client reveals the real reason — **DRM protected**. Their
adaptive/DASH formats are Widevine-encrypted (unplayable by mpv), but YouTube
still serves the legacy progressive **itag 18** (360p H.264+AAC, non-DRM) — and
only the `android` client exposes it, which `android_vr` doesn't.

Fix: query both clients (`player_client=android_vr,android`). yt-dlp merges their
formats; the selector still picks 720p DASH for normal videos (verified
unchanged) and falls back to the muxed 360p (itag 18) for the DRM ones, so they
now play (at 360p). Verified all three resolve to itag 18 with the two-client arg
while normal videos keep 720p. Shipped in a signed release APK.

## Follow-up: playback wedged after seek-then-close (same session, on the phone)

> The app can get into a state, usually after seeking and then immediately
> closing the video, where no more videos will play.

Reproduced on the Pixel with temporary event logging: after a video plays, a
seek + immediate BACK, then playing another shows `play() → EndOfFile → stop()` —
the new video is torn down the instant it starts, and stays that way.

Cause: mpv emits an `MPV_EVENT_END_FILE` whenever the UI issues `stop` (on
leaving playback). But the app only drains player events inside `run_playback`,
which stops running once it leaves the playback screen (`self.playing = None`),
so that `EndFile` (and the `idle-active` after it) sits in mpv's queue. The next
`play` → `loadfile` starts, `run_playback` drains the **stale** `EndFile` first,
reads it as *this* file ending, and stops — and that stop queues another
`EndFile`, so every subsequent play dies the same way. Seeking just makes the
timing line up reliably.

Fix (`LibmpvPlayer::play`, shepherd-media-core): drain the event queue
(`while self.mpv.wait_event(0.0).is_some() {}`) before `loadfile`, so a new
session never inherits a previous one's events. Verified on the phone: the
seek→close→play sequence that used to wedge now plays every time (two cycles).
Shipped to the phone in a signed release APK.

## Follow-up: intermittent first-play error (same session, on the phone)

> investigate that intermittent first-play error too

While debugging the wedge above, a play occasionally showed
`play() → Started → Error → stop()` a few hundred ms in, bouncing back to the
grid; replaying usually worked. Traced it: libmpv2 surfaces an `END_FILE` whose
`error` field is set as `wait_event() -> Err(e)`, which `poll_event` maps to
`PlayerEvent::Error`. So the "error" is the file ending because the stream
failed to open/continue (a flaky googlevideo connection right after start), not
a stray log line (log messages aren't even requested). `run_playback` treated
that identically to a clean end — silently stop and return to the grid, no
feedback, no recovery.

Fix (`run_playback`): distinguish a clean end (EOF/close → leave) from an error.
On an error, retry the same resolved source up to `MAX_PLAYBACK_RETRIES` (2)
before giving up — `PlayingItem` now carries the resolved `source` + external
audio so a retry needs no re-resolve — and only then surface the error as a
status message instead of a silent bounce. Reproducing the transient error
on demand proved impractical (rare, and the idle browse grid makes scripted
`adb` taps unreliable), but the retry is bounded, fires only on `Error`, logs a
`warn`, and healthy playback was verified unaffected. Shipped to the phone.

## Follow-up: initial-load timing, particularly on longer videos

> see about improving initial load timing, particularly on longer videos

Measured the two phases on the Pixel (temporary `TIMING`/codec logs):

| phase | short video (~20 min) | 4-hour video |
| --- | --- | --- |
| yt-dlp resolve (tap → resolved) | 3.4 s | 3.2 s |
| open (play → codec alloc) | 0.33 s | 1.55 s |

So the **yt-dlp resolve (~3 s) dominates every play and is length-independent**;
the length-dependent part is the demux (index read), which adds ~1.2 s for a
very long stream. Two dead ends confirmed by measurement, not assumption:

- mpv demuxer probe tuning (`demuxer-lavf-probesize`/`analyzeduration`) does
  nothing here — an apparent "22× speed-up" was mpv *erroring out* on an
  out-of-range `analyzeduration` value (it wants seconds, not µs); with valid
  units there's no change.
- `demuxer-lavf-o=fflags=+ignidx` (skip the fragmented-MP4 segment index) was
  built and measured on-device: the 4-hour open was 1.70 s with it vs 1.55 s
  without — no gain, so it was dropped rather than ship the seek-precision cost
  for nothing. The 1.55 s open is not dominated by the `sidx` read.

**Fix that landed: prefetch the resolve.** As focus settles on a grid item
(`PREFETCH_DWELL`, 400 ms), its stream is resolved on a worker and cached by
watch URL (`stream_cache`, `STREAM_CACHE_TTL` = 4 h, under googlevideo's URL
expiry); one prefetch runs at a time. Tapping play then reuses the cached
resolution via a pre-filled channel — the same `poll_playback_pending` path,
no resolve wait. A normal (un-prefetched) play also banks its resolution so a
replay is instant. Verified on-device: prefetched 4-hour video went from
selection → `play()` in **5 ms** (was ~3.2 s), first frame ~1.7 s later — total
~1.7 s vs ~4.8 s before. Shipped to the phone.

## Follow-up: port the transient-error retry into the shared session (Linux too)

The Android retry lives in the Android event loop, which bypasses `Session`. The
Linux binary drives playback through the shared `Session::tick` →
`apply_player_event`, where a `PlayerEvent::Error` went straight to `ERROR` +
`RETURNED_TO_MENU` — the same flaky-stream failure mode with no recovery. Added
the bounded retry there (`crates/shepherd-media-core/src/session.rs`): a `Playing`
+ `Error` now restarts the same item in place (emitting `WARNING
reason=playback-retry`, staying `Playing`) up to `MAX_PLAY_RETRIES` (2) before
surfacing the error and returning to the menu; the counter resets when a new item
starts. Two integration tests in `tests/protocol.rs` cover recover-on-transient
and give-up-after-exhaustion. So the Linux front-end now gets the same recovery
the Android app does. (Kept on this branch at the user's request rather than a
separate one.)

## Follow-up: prefetch/play process-id collision (found deploying to the Fire TV)

Deploying the prefetch build to the Fire TV surfaced `youtubedl-android: Process
ID already exists` and a failed play. The youtubedl-android `execute` overload
takes a process id and rejects a second call with a live id; the resolver had
been passing the **watch URL** as that id, which was fine when only a play ever
resolved an item — but prefetch means the same URL can resolve twice at once (a
background prefetch racing the play), so they collided. Two fixes: the resolver
now uses a unique per-call id (`shepherd-resolve-<seq>`), and `start_playback`
adopts an in-flight prefetch's receiver for the same item instead of starting a
second resolve. Re-verified on the 32-bit Fire TV: hardware H.264 decode + audio,
no collision.

## Follow-up: share the retry policy between the two front-ends

The retry existed in two places — the Android event loop and `Session` — with
two `MAX_*_RETRIES = 2` constants that could drift. The *mechanics* genuinely
differ (Android drives its own loop and resolves YouTube async; the Linux binary
goes through `Session` with a protocol emitter), so a full merge would mean
migrating Android onto `Session` — a large refactor for little gain. Instead the
*policy* is now shared: a small `RetryBudget` in `shepherd-media-core::player`
(`try_retry()` / `reset()`, `DEFAULT_MAX = 2`) owns the count + threshold and its
rationale. `Session` holds one (reset on a new item) and the Android
`PlayingItem` holds one; each still performs the restart and reports it its own
way. Unit tests cover the budget; the existing session integration tests are
unchanged (behaviour preserved).
