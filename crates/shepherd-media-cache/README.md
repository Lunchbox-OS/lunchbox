# shepherd-media-cache

The on-disk video cache for media activities: what is cached, under what name,
who is allowed to download it, and what gets thrown away when the disk fills.

Remote library sources — YouTube URLs and plain HTTP files — are downloaded to
`$XDG_CACHE_HOME/shepherd/media/videos/` so a later play comes off local disk:
no buffering, no bandwidth, and the item stays watchable offline.

It also owns the YouTube playlist metadata cache
(`$XDG_CACHE_HOME/shepherd/media/playlists/`), for the same reason: shepherdd
has to know what is in a playlist before it can prefetch it.

## Why it is its own crate

Two processes share this directory. `shepherd-media` reads it when a video
starts and fills it when one finishes; shepherdd prefetches into it in the
background, whether or not the player is running (issue #127).

That is the whole reason this isn't a module of `shepherd-media`: the binary it
used to live in pulls in libmpv, egui, and a gamepad stack, none of which a
daemon should link. Nothing here touches a player — `shepherd-media-core` is
taken with `default-features = false`, so even the mpv-backed `PlayerHandle` is
absent. The player-side wrapper that substitutes cached files at play time stays
behind in `shepherd-media` as `caching_player.rs`.

## Layout on disk

One flat directory, four kinds of file per entry:

| File | Meaning |
|---|---|
| `<key>.<ext>` | the video |
| `<key>.done` | commit sentinel; records the interest key and the selector |
| `<key>.part` | an in-flight direct-HTTP download |
| `<key>.lock` | download claim |
| `<ikey>.played` | the video has been watched (keyed by *interest*, not content) |

A video counts as cached only when its `.done` sentinel exists, so a reader
never picks up a half-written file — yt-dlp in particular leaves unmerged
per-format files around mid-download.

## Two keys

Derived by `shepherd_media_app::cache_key`, shared with the Android cache so the
two agree on what a cached file is called — and, more to the point, on not using
a hash whose output changes between Rust releases.

**Content key** — the first 128 bits of the SHA-256 of the source URL *and* the
yt-dlp format selector. It names the video, its sentinel, its part file, and its
lock.

Item ids are unique only *within* a library, and this is one directory shared by
every library on the device: two libraries that each define `intro` would
otherwise share a file, and whichever downloaded first would be served to both.
That was survivable while one process handled one library at a time. It stops
being survivable the moment shepherdd prefetches every configured library at
once. Keying by URL also dedupes — the same video referenced from two libraries
is downloaded once and hits for both.

The selector is in there because two activities can point at one library with
different `quality` settings. Before, they shared a filename and each launch
saw the other's file as the wrong codec, deleted it, and re-downloaded — a loop
that never converged. Now the two renditions are two files.

**Interest key** — the SHA-256 of the URL alone. It names one file,
`<ikey>.played`, and it exists because a child who watches a video has shown
interest in the *video*, not in a particular rendition of it: watching at 1080p
protects the 480p copy too.

Files written before this change are named after item ids, so nothing will ever
ask for them again. They are left where they are: they carry valid sentinels,
which makes them ordinary LRU candidates that age out on their own. Migrating
them would mean maintaining a rename map forever to save a download that is, by
definition, re-downloadable.

## Two processes, one directory

The `.done` sentinel makes readers safe but arbitrates nothing between writers:
a prefetching daemon and a playing client can both classify a key as absent and
start writing the same `.part` file.

An `flock` on `<key>.lock`, taken non-blocking and held for the download,
settles it. **The loser skips rather than waits** — a prefetch is speculative,
and the play path streams the remote source instead of parking a child in front
of a progress bar they cannot see. After winning the lock the worker re-checks
the cache state, since the process it lost the race to a moment ago may have
just committed exactly this file.

Lock files are never deleted, including by `remove_cached_item`. Removing one
while another process held it open would leave the next claimant locking a fresh
inode, and the mutual exclusion would quietly stop working. They are empty, so
the cost is one directory entry per distinct URL ever downloaded.

## Eviction: watched beats guessed

The ordering itself is `shepherd_media_app::Recency`, and the played marker
behind it is `shepherd_media_app::interest` — both shared with the Android
cache, so the two front-ends spend disk the same way.

The cap is 10 GiB, or `SHEPHERD_MEDIA_VIDEO_CACHE_MAX_BYTES`. What goes first is
a **two-class** ordering, and the classes matter more than the timestamps:

1. **Unwatched** — downloaded speculatively, never played. All of these go
   before any watched file does.
2. **Watched** — has a `.played` marker, ordered by when playback last started.

Plain mtime ordering got this backwards: a prefetch that landed a minute ago
looked "recently used" and outranked a film the child watched last week. A guess
must never cost someone content they chose.

Within the unwatched class, download time is **inverted** — the newest arrival
goes first. Prefetch walks a library in display order, so the newest file is the
one furthest down the list and least likely to be reached next. Evicting the
head instead would have the next prefetch pass immediately re-download it.

`.played` is written when playback actually starts from a cached file, and by
the after-play download path (that download exists *because* the video was
watched — without the marker it would arrive unwatched and be the first thing
the following trim discarded, evicting itself). It is never removed, including
when the video is evicted: interest in a video outlives the file.

The two queue paths differ in what they may spend:

- `queue_prefetch` may recycle space held by **other guesses** and nothing else.
  If everything cached has been watched, the guess is dropped rather than made
  to cost the child a file. This is also what stops the cache going inert once
  it fills: speculative content is always replaceable by more speculative
  content.
- `queue_after_play` follows a video watched to the end, so it is allowed to
  displace the least-recently-watched file.

`cached_path` is a pure lookup and records nothing — enumerating the cache,
which the prefetcher does, must not make every guess look watched.
