# shepherd-media: Android build architecture

## Prompt

> Suggest an architecture for the Android build of shepherd-media. As there can
> only ever be *one* installation per device, it will likely need to accept
> multiple libraries (and their caching options) in a settings page and provide
> some interface to switch between them.

Design discussion captured here per the CLAUDE.md convention. No code was
changed; this is a forward-looking architecture proposal.

## Locked decisions (from the prompting session)

| Decision        | Choice                                      |
|-----------------|---------------------------------------------|
| UI framework    | egui via `eframe` (`android-activity` / `NativeActivity`) |
| Player backend  | libmpv, implemented behind the existing `PlayerHandle` trait |
| YouTube in v1   | Yes — via `youtubedl-android` (yt-dlp bundled through Chaquopy/Python) |

## The constraint that drives everything

On Linux, `shepherd-media` is **stateless and externally driven**: `shepherdd`
spawns it per-activity with `--library <path-or-url>` plus flags
(`--quality`, `--sort-by`, `--connectivity-check`, …). All configuration arrives
as argv at launch; caching is tuned by env vars
(`SHEPHERD_MEDIA_VIDEO_CACHE_MAX_BYTES`); there is no persistent app state.

On Android there is **no `shepherdd`** and only **one install per device**. So
the Android build must grow the one thing the Linux build deliberately lacks:
**persistent, user-managed app state** — the list of libraries, their per-library
caching options, and the currently-selected one. That settings store *is* the
architectural addition; everything else is porting the existing pipeline.

The codebase is already shaped for this:

- `shepherd-media-core` is explicitly platform-agnostic (no Wayland/egui/network/
  subprocess). Confirmed: `library.rs`, `session.rs`, `player.rs` (trait),
  `protocol.rs`, `resolver.rs`, `uri.rs`, `playlist.rs`,
  `youtube_playlist.rs`.
- `Platform::Android` already exists (`library.rs:54`) and `resolver.rs:33`
  selects it under `#[cfg(target_os = "android")]`.
- Playback is already behind the `PlayerHandle` trait (`player.rs:30`), with the
  libmpv impl gated behind the optional `libmpv` feature.
- All Linux-specific concerns (egui, libmpv2, ureq, yt-dlp subprocess, gilrs)
  live in `shepherd-media`, not the core.

So Android is a **third platform binary** beside the Linux one, not a rewrite.

## Recommended crate layout

```
crates/
  shepherd-media-core/      ← unchanged, shared verbatim (already Android-aware)
  shepherd-media/           ← Linux binary (egui + libmpv + clap), unchanged
  shepherd-media-app/       ← NEW: shared, headless "application layer"
  shepherd-media-android/   ← NEW: cdylib + Android glue (the .so the APK loads)
```

**`shepherd-media-app`** is the key new abstraction: lift the parts of today's
`main.rs`/session wiring that are not Linux-specific — settings model, library
list management, "switch active library", caching-policy types, the connectivity
probe, session orchestration — into a platform-agnostic app layer that both a
future refactor of the Linux binary and the Android binary can sit on. The Linux
CLI becomes a thin argv→app-state adapter; the Android UI becomes a thin
settings-store→app-state adapter. This avoids forking the browse/session logic
into two divergent copies.

Minimum viable alternative: have `shepherd-media-android` depend directly on
`shepherd-media-core` and duplicate some session/connectivity glue. Still prefer
`-app`, since the multi-library settings logic is new code either way and that is
its natural home.

## The new piece: persistent settings model

This is what replaces argv. Persist as TOML/JSON in app-private storage
(`Context.filesDir`), edited via a settings screen:

```toml
# app-settings.toml  (managed by the app, NOT hand-authored)
schema_version = 1
active_library = "kids-movies"

[[libraries]]
id = "kids-movies"
label = "Kids Movies"
source = { kind = "saf-toml", uri = "content://.../movies.toml" }
# other source kinds: "youtube-playlist" {url}, "http-toml" {url}, "m3u" {uri}

[libraries.caching]
mode = "queue-after-play"     # off | queue-after-play | queue-all
max_bytes = 5_368_709_120     # per-library budget
posters = "wifi-only"         # always | wifi-only | never
quality = "720p"

[[libraries]]
id = "lofi"
label = "Lofi Beats"
source = { kind = "youtube-playlist", url = "https://youtube.com/playlist?list=UU..." }
```

Each entry maps onto existing concepts: `source.kind` is exactly the dispatch the
Linux binary already does on the `--library` argument (TOML file vs `.m3u` vs
YouTube playlist URL), and `caching.mode` maps onto the two existing `VideoCache`
strategies (`queue_all` = Option A, `queue_after_play` = Option B, plus `off`).
The settings model is essentially a *persisted, multi-instance* version of flags
that already exist.

One real change vs Linux: Android cannot keep a stable `file://` path or re-read
an arbitrary file later. To "add a library from a file" you take a **SAF
persistable URI permission** and store the `content://` URI, re-resolved through
`ContentResolver` on each launch.

## UI navigation

Reuse the existing fullscreen, touch- and gamepad-friendly egui UI (poster grid +
auto-hiding playback overlay). Add two screens the Linux build never needed:

- **Library switcher** — entry point with no `shepherdd` to pick for you. Tap a
  library → its poster grid. (Single-library setups skip straight to the grid.)
- **Settings** — add/remove/reorder libraries, pick source (SAF file picker,
  paste YouTube/URL), set per-library caching/quality/poster policy and the
  global cache budget.

Grid → player flow unchanged. Stack: `Settings ⇄ LibrarySwitcher → Grid → Player`.

## Player backend (libmpv)

Implement a new `PlayerHandle` (`player.rs:30`) backed by libmpv, rendering to a
GL surface, mirroring mpv-android. The existing `bind_gl`/`render`/
`set_redraw_callback` shape on the trait already matches this model. Heaviest
packaging task: build/bundle `libmpv.so` for `arm64-v8a` (+ `x86_64` for
emulator).

## YouTube (v1, via youtubedl-android)

The Linux binary shells out to `yt-dlp` on `PATH`; Android has no `PATH`/
subprocess story. Use `youtubedl-android` (yt-dlp through Chaquopy/Python) for
exact parity, behind the **same metadata-cache interface** the desktop already
uses (`youtube_playlist.rs` / `youtube.rs`, 6-hour TTL, stale-as-fallback).
Trade-off accepted: larger APK from the bundled Python runtime.

## Caching, adapted

Keep the three-tier design (posters, playlist metadata, videos) but:

- Replace `$XDG_CACHE_HOME/shepherd/media/...` with `Context` dirs. Videos that
  must survive system pressure belong in `filesDir`, **not** `cacheDir` (Android
  can purge `cacheDir`).
- Make cap and mode **per-library settings** instead of an env var, plus a
  **device-wide budget** so multiple libraries can't collectively blow up
  storage. The LRU-by-mtime eviction in `video_cache.rs` carries over directly.
- Replace `ureq` poster/HTTP fetches with a client that has working TLS on the
  NDK targets (`reqwest`+`rustls`, or `ureq` with rustls if it builds clean).
- Honor `wifi-only` policy via Android connectivity APIs (JNI), feeding the same
  connectivity-probe gate the browse UI already has (`connectivity.rs`).

## Build & packaging

- `shepherd-media-android` as `crate-type = ["cdylib"]`, built with `cargo-ndk`
  for `arm64-v8a` (+ `x86_64`).
- Thin Gradle/Android project producing the APK. With egui, the Rust cdylib +
  `NativeActivity` is basically the whole app; JNI shims only for SAF, the file
  picker, connectivity, and the youtubedl-android bridge.
- CI: add an NDK build job. Document NDK + `cargo-ndk` + prebuilt `libmpv.so` in
  `CONTRIBUTING.md` / `docs/INSTALL.md`.

## Suggested phasing

1. **Extract `shepherd-media-app`** (settings model, library-list/switch logic,
   caching-policy types) — pure Rust, testable on desktop, no Android yet.
2. **`shepherd-media-android` skeleton**: eframe `NativeActivity`, settings
   persisted to `filesDir`, library switcher + settings screens, local/HTTP/m3u
   libraries, stub player.
3. **libmpv `PlayerHandle`** + GL surface → real playback.
4. **Video/poster cache** on Android paths with per-library policy.
5. **YouTube** via youtubedl-android, reusing the metadata-cache interface.

## Implementation status

- **Step 1 — `shepherd-media-app` crate: done.** Pure-Rust application layer
  with the persistent settings model (`AppSettings`, `LibraryEntry`,
  `LibrarySource`, `CachingSettings`), library management (add/remove/reorder/
  select-active with validation), caching policy types (`CacheMode`,
  `PosterPolicy`, `Quality` mirroring the Linux binary), and atomic TOML
  load/save. 25 unit tests; builds, tests, clippy, and fmt clean. The crate has
  no UI/network/Android dependency so it is testable on the desktop.
- **Step 2 — `shepherd-media-android` crate + APK build: done.** A `cdylib`
  hosting the cross-platform eframe UI (`MediaApp`) over core + app: library
  switcher, settings page (add/remove/reorder/select-active, per-library cache
  mode / quality / poster policy / size cap), add-library form, and a
  placeholder grid; settings persist to the app's private storage. A
  `StubPlayer` stands in for the playback backend. The same UI runs on the host
  via the `desktop_preview` example. The crate cross-compiles for
  `aarch64-linux-android` (exports `android_main` / `ANativeActivity_onCreate`),
  and the pure-`NativeActivity` Gradle project under `android/` builds an
  installable APK end-to-end (Rust → cargo-ndk → AGP), verified with NDK
  27.2.12479018 / cargo-ndk 4.1.2 / Gradle 8.10.2 / AGP 8.7.3.
- **Step 3 (partial) — source resolution + real browse grid: done.** A
  `resolve` module turns a `LibrarySource` into a parsed
  `shepherd_media_core::Library` for the tractable kinds: local/`file://` TOML,
  HTTP(S) TOML, and `.m3u`/`.m3u8` (local or HTTP), via `ureq`+rustls. It runs
  on a worker thread (Android forbids network on the UI thread), and the grid
  screen polls the result and lists the real items with a loading/error state.
  `content://` SAF and YouTube sources return a typed `Unsupported` error
  pending their bridges. 9 crate tests; the resolver (incl. rustls/ring)
  cross-compiles for `aarch64-linux-android` and the APK still builds.
- Remaining: libmpv-backed `PlayerHandle` + embedded playback (the long pole —
  needs a prebuilt Android `libmpv.so` from an mpv/ffmpeg cross-compile); poster
  image fetching/caching in the grid; YouTube resolution via youtubedl-android;
  and the SAF / connectivity / storage-path JNI bridges.

## Key source references

- Core (shared verbatim): `crates/shepherd-media-core/src/{library,session,player,protocol,resolver,uri,playlist,youtube_playlist}.rs`
- Platform resolution: `resolver.rs:33`, `Platform` enum `library.rs:54`
- Player trait: `player.rs:30`
- Caching to port: `crates/shepherd-media/src/{video_cache,posters,youtube,connectivity}.rs`
- Linux invocation reference: `docs/shepherd-media.md`, `config.example.toml`
