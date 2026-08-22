# shepherd-media-app

Platform-agnostic **application layer** for `shepherd-media`.

On Linux, `shepherd-media` is (almost) stateless: `shepherdd` spawns it per
activity with a `--library` argument plus flags, so there is nothing to persist —
the one exception being the opt-in resume positions below. The Android build has
no `shepherdd` and only one install per device, so it must keep its own state:
the set of configured libraries, each library's caching options, and which one is
currently selected.

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
- **Video-cache eviction policy** (`lru.rs`, `interest.rs`) — [`Recency`], the
  two-class ordering both front-ends' video caches evict by (every file nobody
  has watched goes before any file somebody did, and among the unwatched the
  newest download goes first), plus the `.played` marker that distinguishes the
  two. Only the *policy* is shared: each cache scans its own directory, applies
  its own filters, and does its own bookkeeping.
- **Persistence** — load/save the settings as TOML to app-private storage
  (e.g. Android `Context.filesDir`). Writes are atomic (temp file + rename).
- **Resume state** (`resume.rs`) — [`ResumeState`] (a position per item plus the
  last item watched, per library), its forget-it policy, TOML persistence, and
  [`ResumeTracker`], which turns a per-frame stream of player positions into
  batched writes. Used by *both* front-ends for the opt-in resume feature
  (`--resume` on Linux, the per-library toggle on Android), which is why it is
  here even though the Linux binary is otherwise stateless. Where the file lives
  stays with each binary (`$XDG_STATE_HOME` vs app-private storage).

## What lives elsewhere

Turning a [`LibraryEntry`] into a resolved `shepherd_media_core::Library` needs
network I/O (YouTube), filesystem access, and — on Android — a `ContentResolver`
to read a `content://` URI. That resolution is platform-specific and stays in the
platform binary. This crate only models and manages the *settings*.

Session orchestration and the connectivity probe are candidates to migrate here
later so both the Linux and Android binaries can share them; today they still
live in `shepherd-media`.
