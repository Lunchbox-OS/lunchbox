# Video precaching for remote library sources

**Branch:** `u/albert/9/media-launcher`  
**Issues:** #9 (media type and libraries), #34 (PR: Media launcher and libraries)

## Summary

Added background video file caching for remote library sources (YouTube and
direct HTTP), implemented as two complementary strategies:

- **Option A** — on browse launch, every remote item in the library is queued
  for background download so that subsequent views play from local cache.
- **Option B** — when a video finishes playing naturally (EOF), its URL is
  queued for download so the *next* time that item is selected it plays locally.

A single-shot `play` command also benefits from cache lookups (no queue_all).

## Files changed

### New
- `crates/shepherd-media/src/video_cache.rs` — `VideoCache` struct (cache dir
  management, background download queue), `CachingPlayer` wrapper that
  implements `PlayerHandle`, and private download worker functions for yt-dlp
  and HTTP sources.

### Modified
- `crates/shepherd-media/src/main.rs` — wires `VideoCache` and `CachingPlayer`
  into `run_browse` (Option A: `queue_all` + `CachingPlayer`) and `run_play`
  (cache lookup only).

## Design decisions

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

- **`find_cached_file` scans for `<item_id>.*` excluding `.part`.**
  yt-dlp writes intermediate files with a `.part` suffix and renames to the
  final extension on completion. This makes the presence-check reliable across
  all container formats without enumerating expected extensions.

- **Cache dir: `$XDG_CACHE_HOME/shepherd/media/videos/`.** Follows the same
  XDG convention as the playlist metadata cache in `youtube.rs`.

- **HTTP download uses `.part` then rename.** Matches yt-dlp's own convention
  so `find_cached_file` works identically for both source types.

- **`stop()` clears `last_played`.** An explicit stop (user closes mpv window
  or the session issues `stop`) does not trigger an Option B download. Only a
  natural EOF does.
