# Validating the scored cache and the new extractor args on Android hardware

> Branch: `feat/media-kind` (PR #142)
> Follows: `2026-08-22 002 media-cache-eviction-weights-watched-against-age.md`
> and `2026-08-22 003 youtube-403-and-prefetch-failure-handling.md`

## Prompt

> #142 is checked out. do the Android on-device validation

## Why there was anything left to validate

`2026-08-22 001 media-kind-scope.md` records an on-device pass at 1150dec: the
shared `content_key` naming, `.played` markers, and eviction, checked on a Pixel
10a. Four commits landed after it, and three of them change code Android runs:

| Commit | Android-visible change |
| --- | --- |
| 9b9c997 | `Recency` (two classes) → `Score` (one time axis, eroding grace); a second marker, `<key>.seen`, written by `store()` |
| a8476f4 | `YOUTUBE_EXTRACTOR_ARGS` — shared verbatim with `shepherd-media-android`'s `resolve_stream_url` |
| 4bb9705, ab57597 | daemon-side only; no Android surface |

So the Android cache had been rewritten under a new policy and its YouTube
resolver had its player-client list changed, with only `cargo check` behind both.

**Re-checked at aeb0b02** (`service.media.cache_max_bytes`), which touches
`shepherd-media-cache`, the config crates and the Linux adapter, and none of the
three crates Android builds from — `shepherd-media-core`, `-app`, `-ui`. The
build, the lint and all three on-device suites were re-run there anyway and are
unchanged; the end-to-end app pass below was not repeated, because nothing it
exercises moved. Note that the cap it makes configurable on Linux has always
been per-library and settable in the app's own settings UI on Android
(`CachingSettings.max_bytes`), so there is no Android gap to close.

## Build

Same NDK (`/opt/android-sdk/ndk/27.2.12479018`), same Pixel 10a (arm64-v8a,
`ro.product.cpu.abilist` = `arm64-v8a` only) over USB.

- `cargo ndk -t arm64-v8a build -p shepherd-media-android` links the cdylib.
- `cargo ndk -t arm64-v8a clippy -p shepherd-media-android --all-targets -- -D warnings` is clean.
- Gradle `assembleDebug` packages `libshepherd_media_android.so` for **both**
  `arm64-v8a` (14,219,176 B) and `armeabi-v7a` (10,191,244 B), each matching the
  release artifact `cargoNdkBuild` had just produced. The 32-bit ABI still links
  with the new `Score`/`Standing` code.

  Check the staged file, not just the APK listing. The first `assembleDebug`
  here died on `No space left on device` *after* `cargoNdkBuild` had written
  both `.so`s, and the APK from the retry carried an `armeabi-v7a` entry of a
  different size than the artifact on disk — a leftover from an earlier build.
  A clean run packages exactly what it just compiled; the two sizes above are
  from that run.

## Unit tests on the device

Same workflow as last time — a cargo-ndk-built test binary run straight off
`/data/local/tmp`, `TMPDIR` set, `LD_LIBRARY_PATH` at the vendored libmpv for
the binaries that link it:

| Crate | Result | Was |
| --- | --- | --- |
| `shepherd-media-app` | **94/94** | 81/81 |
| `shepherd-media-android` | **30/30** | 28/28 |
| `shepherd-media-core` (`--lib`) | **61/61** | not previously run |

The growth is the new policy: `a_play_old_enough_to_have_lost_its_grace_stops_protecting_the_file`
and `a_re_download_is_not_a_fresh_arrival` both pass on bionic/aarch64, which is
the only place the erosion half of the policy *can* be exercised on Android (see
"What the app still cannot show" below).

`shepherd-media-core` was added to the on-device set because 003 changed
`parse_flat_playlist` there and Android is the only front-end that calls it.
Its test binary needs `RUSTFLAGS="-L .../vendor/libmpv/arm64-v8a"`; the crate
links `-lmpv` and, unlike `shepherd-media-android`, has no `build.rs` adding the
vendored directory to the search path.

**No 32-bit runtime coverage.** The Pixel 10a is arm64-only (a pushed
`armeabi-v7a` binary is rejected: "not executable: 32-bit ELF file"), and the
Fire TV stick at `192.168.0.4` was unreachable during this pass. The 32-bit side
is still only build-checked.

## End-to-end in the app

Driven against a loopback library over `adb reverse tcp:8099 tcp:8099` with a
three-item TOML library served from the host and `settings.toml` written into
`/data/data/<pkg>/files` via `run-as` (debug build, as before). Three
distinctly-sized H.264 files: `one` 50,970 B, `two` 5,398 B, `three` 794,074 B.
`mode = "queue-after-play"`.

| Step | Observed |
| --- | --- |
| Play `two` (uncached) | streams, then `store()` writes `dfe4a4b75c3d1793b4cab38287ffd677.mp4` — the host-computed `content_key(url, "")` — plus `.played` **and the new `.seen`** under the same key |
| Replay `two` | zero HTTP requests; `.mp4` mtime unchanged; `.played` mtime moves forward; **`.seen` mtime unchanged** — `mark_seen` is write-once on real storage, not just in tempdirs |
| Cap lowered to 200,000, play `one` | `three.mp4` (the earliest-played, and the largest) is evicted; `one` and `two` survive; total `.mp4` bytes 56,368 ≤ cap |
| After that eviction | **both** of `three`'s markers survive — `.played` *and* `.seen` |
| Cap restored, replay `three` | re-downloads; `.mp4` and `.played` stamped now, **`.seen` still 1787447376** — the first sighting from before the eviction. "A re-download is not a fresh arrival", end to end |

That last row is the one worth keeping: the `.seen` marker outliving both the
file and an eviction is what stops a churned item from competing as though the
parent had added it today, and it is now confirmed against real app storage
rather than a `tempfile`.

### What the app still cannot show

The eroding-grace comparison needs a file *nobody has watched* to compete
against one watched long ago, and nothing on Android produces an unwatched file
until `CacheMode::QueueAll` prefetches. Every eviction the app can be driven
into is watched-vs-watched, which the old two-class ordering would have decided
the same way. So the headline of 002 is covered by the on-device unit tests and
not by the app — the same gap 001 recorded for the two-class ordering, for the
same reason.

## YouTube, with the new player clients

003 changed `YOUTUBE_EXTRACTOR_ARGS` from `android_vr,android` to
`default,android`, and e334fc2 has since confirmed on Linux that the old value
broke *playback* of any uncached video, not just caching. Android is a third
consumer of the constant and an independent check: it uses neither mpv's
`ytdl_hook` nor the host's yt-dlp, but hands the constant to youtubedl-android's
own bundled copy. That binding logs `yt-dlp payload refreshed from GitHub` at
startup, so the device is running a current yt-dlp, not the one vendored at
build time.

A two-item playlist via `watch_videos?video_ids=6sk2j8gFdoU,B7UmUX68KtE` — the
exact pair 003 diagnosed against — resolved and played:

| Video | Kind | Result |
| --- | --- | --- |
| `6sk2j8gFdoU` | ordinary upload (403 under `android_vr`) | plays, `mpv is decoding video with mediacodec (zero-copy)`, real frames on screen |
| `B7UmUX68KtE` | DRM "full episode" (only itag 18 works) | plays, same path |

Both halves of the "add, don't replace" fix hold on Android's yt-dlp, which is a
different build from the host's — and this is the streaming path throughout,
since `mode = "off"` for that library. The playlist fetch also exercised the 003
`parse_flat_playlist` change live: two entries, including one with a non-ASCII
title (`Pöpcørn | …`), came back as tiles.

## Incidental

**The box ran out of disk mid-run** (`/` at 100%, gradle failed with
`No space left on device`). `target/` was 33 GB. Removing
`target/debug/incremental`, `target/debug/examples`, and the per-ABI Android
`incremental` directories freed ~2 GB, which was enough; `/` is still at 99%.
Nothing non-rebuildable was touched, but a `cargo clean` is overdue on this
machine.

**`CachingSettings.quality` is spelled `"480p"`, not `"q480"`** in
`settings.toml` — the `Quality` enum renames to the display form. The app
surfaces the mismatch properly (an orange TOML parse error naming the line and
listing `best`, `1080p`, `720p`, `480p`), which is how it was caught.

## State left behind

The debug APK is installed on the Pixel (as it already was), and the test
`settings.toml` and video cache were removed, returning `files/` to the empty
directory it started as. `/data/local/tmp/mv`, the `adb reverse`, and the host
HTTP server are gone.
