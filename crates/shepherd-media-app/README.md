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
- **Cache-file naming** (`cache_key.rs`) — [`content_key`] and
  [`interest_key`], the truncated SHA-256 names both front-ends' video caches
  give their files. Deliberately not `DefaultHasher`, whose output is
  unspecified across Rust releases: a toolchain bump would rename every file in
  every cache, silently re-downloading everything.
- **Video-cache eviction policy** (`lru.rs`, `interest.rs`) — [`Score`], the
  single value both front-ends' video caches evict by, plus the `.played` and
  `.seen` markers it is computed from. Watching a file buys it a grace period
  against being displaced, which erodes at one day per day, so protection is
  real but not permanent; unwatched files are ordered by when the item entered
  the library, with its position in that library breaking ties. Only the
  *policy* is shared: each cache scans its own directory, applies its own
  filters, and does its own bookkeeping.
- **Persistence** — load/save the settings as TOML to app-private storage
  (e.g. Android `Context.filesDir`). Writes are atomic (temp file + rename).
- **Resume state** (`resume.rs`) — [`ResumeState`] (a position per item plus the
  last item watched, per library), its forget-it policy, TOML persistence, and
  [`ResumeTracker`], which turns a per-frame stream of player positions into
  batched writes. Used by *both* front-ends for the opt-in resume feature
  (`--resume` on Linux, the per-library toggle on Android), which is why it is
  here even though the Linux binary is otherwise stateless. Where the file lives
  stays with each binary (`$XDG_STATE_HOME` vs app-private storage).

- **SponsorBlock buckets** (`sponsorblock.rs`) — the on-disk half of the segment
  lookup (issue #159): one file per hash prefix holding the server's own bytes,
  a TTL judged from the file's mtime, and the stale-on-failure fallback that
  keeps an offline device skipping. Split from the HTTP fetch exactly as
  the poster cache is, so both front-ends spend the same bytes on the same
  schedule. `cache_key::sponsorblock_prefix` is the bucket name, and it is the
  service's convention rather than ours.

## What lives elsewhere

Turning a [`LibraryEntry`] into a resolved `shepherd_media_core::Library` needs
network I/O (YouTube), filesystem access, and — on Android — a `ContentResolver`
to read a `content://` URI. That resolution is platform-specific and stays in the
platform binary. This crate only models and manages the *settings*.

Session orchestration and the connectivity probe are candidates to migrate here
later so both the Linux and Android binaries can share them; today they still
live in `shepherd-media`.
