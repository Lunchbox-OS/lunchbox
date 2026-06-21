# YouTube poster cache misses thumbnails when offline (#64)

> Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/64>
> "shepherd-media YouTube cache is missing thumbnails when offline"

## Prompt

> Fix #64. You may run `shepherd-media` directly in this environment with
> <https://www.youtube.com/playlist?list=PL6D326BFD2E6696FC> as the playlist.
> See the Steam debugging logs for how to correctly firewall the connection to
> emulate the offline condition once you have a cache.

## Root cause

`shepherd-media` has two on-disk caches for YouTube libraries, both with a
6-hour TTL (`CACHE_TTL_SECS`):

- **Playlist metadata** (`youtube.rs`) — already handles offline correctly: a
  stale entry is kept and, if the live `yt-dlp` fetch fails, returned anyway
  (`CacheFreshness::Stale` fallback, added in commit `bcee27f`).
- **Posters / thumbnails** (`posters.rs`) — did **not**. `load_from_disk_cache`
  returned `None` once the cached file aged past the TTL, so `prefetch` treated
  a stale entry identically to a miss: it attempted an HTTP fetch and, on
  failure, produced a placeholder.

So a device that had been offline longer than 6 hours would still load the
playlist (stale metadata fallback) but lose every thumbnail it had already
cached — exactly the reported symptom.

Note: the poster URL is the *derived* `https://i.ytimg.com/vi/<id>/hqdefault.jpg`
(see `youtube_playlist::build_item`), because `yt-dlp --flat-playlist` leaves
the scalar `thumbnail` field null and only fills a `thumbnails[]` array the
binary doesn't parse. That derived URL is stable (no expiring `sqp=`/`rs=`
signed params), so the URL-hash cache key is stable across runs — the cache key
was never the problem, only the TTL eviction was.

## Fix

`crates/shepherd-media/src/posters.rs`, mirroring the playlist cache:

- `load_from_disk_cache` now returns `Option<(PosterBytes, CacheFreshness)>` —
  it reports `Stale` instead of discarding an expired-but-readable entry, and
  only returns `None` on a true miss.
- A pure `resolve_remote(cached, fetch)` decides the outcome:
  `CacheHit` (fresh) → `Fetched` (miss/stale + live fetch ok) →
  `StaleFallback` (live fetch failed, stale bytes available) → `Failed`. Keeping
  it pure makes the offline-fallback decision unit-testable without network/FS.
- `prefetch` maps the `Resolution` to logging + in-memory/disk-cache writes.

## Verification

Reproduced and confirmed the fix by running the real binary against the issue's
playlist. `posters::prefetch` runs synchronously *before* any window opens, so
its `tracing` output reflects the fix without needing the GUI to render.

Offline was emulated with a network namespace (clean "yanked cable" that leaves
the agent's own model-API path intact — the cgroup-scoped nftables approach from
`docs/ai/history/2026-05-31 001 steam-offline-cloud-modal-investigation.md` is
the production-representative alternative):

```sh
sudo ip netns add offline && sudo ip netns exec offline ip link set lo up
# populate cache online, then backdate it past the 6h TTL:
touch -d "7 hours ago" ~/.cache/shepherd/media/posters/*.bin
sudo ip netns exec offline sudo -u shepherd-dev env HOME=... RUST_LOG=shepherd_media=debug \
  ./target/debug/shepherd-media browse --library "https://www.youtube.com/playlist?list=PL6D326BFD2E6696FC"
```

- **Before:** `remote poster fetch failed: … i.ytimg.com … ` for every item →
  0 posters loaded → placeholders.
- **After:** `remote poster fetch failed: … ; using stale cache` for every item
  → all posters served from the stale cache.
- Online run still fetches fresh and rewrites the cache (verified by fresh
  mtimes on the `.bin` files afterward).

Added 5 unit tests covering the fresh / miss+ok / stale+ok / stale-offline /
miss-offline decisions. `cargo test`, `cargo clippy --all-targets`, and
`cargo fmt --all --check` all clean.
