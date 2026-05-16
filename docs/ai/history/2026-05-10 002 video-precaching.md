# Video precaching for remote library sources

**Branch:** `u/albert/9/media-launcher`  
**Issues:** #9 (media type and libraries), #34 (PR: Media launcher and libraries)

## Summary

Added background video file caching for remote library sources (YouTube and
direct HTTP), implemented as two complementary strategies:

- **Option A** — on browse launch, every remote item in the library is queued
  for background download so that subsequent views play from local cache.
  The worker **skips** items if the cache is already at capacity to avoid
  displacing existing content.
- **Option B** — when a video finishes playing naturally (EOF), its URL is
  queued for download so the *next* time that item is selected it plays locally.
  The worker **evicts** LRU files after downloading to stay within the size cap.

A single-shot `play` command also benefits from cache lookups (no queue_all).

Also added: LRU size-cap eviction, a `.done` sentinel to prevent serving
partially-downloaded files, an offline mode that hides online-only items,
background connectivity checking, and a configurable `--quality` CLI flag.

## Files changed

### New
- `crates/shepherd-media/src/video_cache.rs` — `VideoCache` struct (cache dir
  management, background download queue, LRU eviction) and `CachingPlayer`
  wrapper that implements `PlayerHandle`.
- `crates/shepherd-media/src/connectivity.rs` — background TCP connectivity
  checker; exposes an `Arc<AtomicBool>` that the UI reads each frame.

### Modified
- `crates/shepherd-media/src/main.rs` — wires `VideoCache` and `CachingPlayer`
  into `run_browse` (Option A: `queue_all` + `CachingPlayer`) and `run_play`
  (cache lookup only); handles `--connectivity-check` and `--quality` flags.
- `crates/shepherd-media/src/cli.rs` — added `Quality` enum and global
  `--quality` flag (default `1080p`); added `--connectivity-check` to `Browse`.
- `crates/shepherd-media-core/src/player.rs` — `LibmpvPlayer::new` accepts
  `ytdl_format: &str` instead of hardcoding a format string.
- `crates/shepherd-media/src/ui/mod.rs` — `run()` accepts `online` and `cache`
  args; `BrowseApp` filters visible items per-frame based on connectivity.
- `crates/shepherd-media/src/ui/grid.rs` — `draw()` takes a pre-filtered
  `&[Item]` slice; sources library title from `session.library()`.
- `crates/shepherd-media/Cargo.toml` — added `filetime = "0.2"`.

## Design decisions

### Cache structure

- **No core changes.** `VideoCache` and `CachingPlayer` live entirely in the
  Linux binary crate (`shepherd-media`). The platform-agnostic core is
  unchanged.

- **`CachingPlayer` wraps `Box<dyn PlayerHandle>`.** The wrapper intercepts
  `play()` to substitute a local cached path when available, and `poll_event()`
  to trigger Option B on EOF. All other calls delegate to the inner player.
  No changes to the `PlayerHandle` trait signature were needed.

- **`url_to_id` reverse map.** `CachingPlayer::new` builds a
  `HashMap<String, String>` (source URL → item ID) from the library at
  construction time so that `play(&source)` can identify which item is being
  played without modifying the `PlayerHandle::play` signature.

- **Sequential download worker.** A single background thread drains an
  unbounded `mpsc` channel. Sequential processing avoids saturating the
  network on large playlists; the tradeoff is that items near the end of a
  large library take longer to become cached. The worker re-checks
  `find_cached_file` at the start of each item to skip duplicates (Option B
  can re-queue an item that Option A already downloaded).

- **Cache dir: `$XDG_CACHE_HOME/shepherd/media/videos/`.** Follows the same
  XDG convention as the playlist metadata cache in `youtube.rs`.

- **HTTP download uses `.part` then rename.** Matches yt-dlp's own convention.

- **`stop()` clears `last_played`.** An explicit stop (user closes mpv window
  or the session issues `stop`) does not trigger an Option B download. Only a
  natural EOF does.

### LRU eviction

- **Size cap defaults to 10 GiB**, overridable via
  `SHEPHERD_MEDIA_VIDEO_CACHE_MAX_BYTES`.

- **LRU order via mtime.** `CachingPlayer::play` calls `filetime::set_file_mtime`
  on a cache hit to update the file's access time. `evict_to` sorts
  `CacheEntry` by mtime ascending and deletes oldest first.

- **Option A vs Option B eviction semantics differ deliberately.** In steady
  state (cache full, same library, same play history) a uniform pre-eviction
  policy would cause perpetual churn — every Option A prefetch would displace
  an existing file, which then becomes the next prefetch target. The split
  prevents this: Option A (`evict_after: false`) skips downloads when at
  capacity; Option B (`evict_after: true`) always downloads then evicts to cap.

- **Library-scoped cleanup is not implemented.** Cached files for items removed
  from the library are only reclaimed via opportunistic LRU eviction. This was
  considered acceptable for the current use-case.

### `.done` sentinel

- **Problem:** yt-dlp's `bestvideo+bestaudio` format selector downloads video
  and audio as separate streams then merges them. During the merge phase,
  intermediate per-format files (`<item_id>.f123.mp4`) exist on disk alongside
  the partial output, and `find_cached_file`'s `<item_id>.*` glob would match
  them, causing mpv to play a video-only track.

- **Fix:** `download_youtube` writes an empty `<item_id>.done` sentinel file
  only after yt-dlp exits 0 (by which point yt-dlp has deleted all intermediate
  files and the merged container is complete). `find_cached_file` requires the
  sentinel to exist before returning a path. `collect_cache_entries` (used by
  eviction) likewise only includes files that have a corresponding `.done`.
  `evict_to` deletes the sentinel together with the video file.

### Offline mode

- **`visible_items()` filters per frame** in `BrowseApp`. When online
  (`online.load() == true`), every item with a platform-compatible source is
  shown. When offline, only items playable without the network are shown:
  `ClassifiedUri::Local` sources are always shown; remote sources are shown only
  if `VideoCache::cached_path(&item.id).is_some()`.

- **`focused` is clamped to `visible.len() - 1` each frame** so that going
  offline (shrinking the visible list) never leaves the cursor out of bounds.

- **Connectivity check via `--connectivity-check <URL>`** (same format as
  shepherdd's `internet.check` config: `https://…`, `http://…`,
  `tcp://host:port`). The `connectivity` module spawns a background thread
  that does a TCP connect to the derived host:port every 10 seconds with a 3 s
  timeout. The result is stored in `Arc<AtomicBool>`, which the UI reads
  without blocking. When the flag is absent, the atomic is initialised to
  `true` (optimistic online assumption).

### Configurable quality

- **`--quality` global CLI flag** (values: `best`, `1080p`, `720p`, `480p`;
  default `1080p`). Maps to a yt-dlp format selector string used both by mpv's
  `ytdl-format` property and by the cache's `yt-dlp --format` argument, so
  browsing, direct play, and background downloads all use the same quality
  setting.

- **`ytdl_format: &str` is passed through** `run_play`/`run_browse` →
  `LibmpvPlayer::new` and `VideoCache::new` → download worker thread. It is
  not stored in `VideoCache` (the field was removed as dead code after the
  value is moved to the worker at construction).
