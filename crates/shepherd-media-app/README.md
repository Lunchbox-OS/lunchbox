# shepherd-media-app

Platform-agnostic **application layer** for `shepherd-media`.

On Linux, `shepherd-media` is stateless: `shepherdd` spawns it per activity with
a `--library` argument plus flags, so there is nothing to persist. The Android
build has no `shepherdd` and only one install per device, so it must keep its own
state: the set of configured libraries, each library's caching options, and which
one is currently selected.

This crate is the home for that state. It is pure Rust with no UI, no network, no
process-spawning, and no Android dependency, so it can be unit-tested on the
desktop and reused by any platform binary.

## Contents

- **Settings model** (`settings.rs`) — [`AppSettings`], a list of
  [`LibraryEntry`] values, and the [`LibrarySource`] enum describing where each
  library comes from (a SAF `content://` TOML file, an `http(s)://` TOML file, an
  `.m3u`/`.m3u8` playlist, or a YouTube playlist URL). The source kinds mirror
  the dispatch the Linux binary already performs on its `--library` argument.
- **Library management** — add / remove / reorder / select-active operations on
  `AppSettings`, with validation and a typed [`SettingsError`].
- **Caching policy** (`quality.rs`) — [`CacheMode`] (mirrors the two
  `VideoCache` strategies in `shepherd-media`, plus `off`), [`PosterPolicy`],
  and [`Quality`] (mirrors the `--quality` presets, including `ytdl_format`).
- **Persistence** — load/save the settings as TOML to app-private storage
  (e.g. Android `Context.filesDir`). Writes are atomic (temp file + rename).

## What lives elsewhere

Turning a [`LibraryEntry`] into a resolved `shepherd_media_core::Library` needs
network I/O (YouTube), filesystem access, and — on Android — a `ContentResolver`
to read a `content://` URI. That resolution is platform-specific and stays in the
platform binary. This crate only models and manages the *settings*.

Session orchestration and the connectivity probe are candidates to migrate here
later so both the Linux and Android binaries can share them; today they still
live in `shepherd-media`.
