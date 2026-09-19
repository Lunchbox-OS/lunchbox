# lunchbox-media-cache

The on-disk video cache for media activities: what is cached, under what name,
who is allowed to download it, and what gets thrown away when the disk fills.

Remote library sources — YouTube URLs and plain HTTP files — are downloaded to
`$XDG_CACHE_HOME/shepherd/media/videos/` so a later play comes off local disk:
no buffering, no bandwidth, and the item stays watchable offline.

It also owns the YouTube playlist metadata cache
(`$XDG_CACHE_HOME/shepherd/media/playlists/`), for the same reason: lunchboxd
has to know what is in a playlist before it can prefetch it.

It also fetches **SponsorBlock buckets** (issue #159), for the same reason: the
player looks a video's segments up when it starts playing, and lunchboxd warms
the same buckets alongside the videos it prefetches, so a library filled while
online still skips when it is played offline. Only the HTTP request is here —
the disk policy is `lunchbox_media_app::BucketStore` and the decisions are
`lunchbox_media_core::sponsorblock`, both shared with the Android app.

## Why it is its own crate

Two processes share this directory. `lunchbox-media` reads it when a video
starts and fills it when one finishes; lunchboxd prefetches into it in the
background, whether or not the player is running (issue #127).

That is the whole reason this isn't a module of `lunchbox-media`: the binary it
used to live in pulls in libmpv, egui, and a gamepad stack, none of which a
daemon should link. Nothing here touches a player — `lunchbox-media-core` is
taken with `default-features = false`, so even the mpv-backed `PlayerHandle` is
absent. The player-side wrapper that substitutes cached files at play time stays
behind in `lunchbox-media` as `caching_player.rs`.

## Skipping the TTLs on request (issue #165)

Every cache here answers "is this fresh enough" with a clock, which is right for
the hourly sweep and wrong for an administrator who has just changed something.
`refetch_playlist` and `SponsorBlockCache::refresh` are the same fetches with
the freshness check removed, and `clear_all_failures` forgets every download
cooldown in the directory at once.

All three **re-fetch rather than delete-then-fetch**, which is the whole design
constraint. Every cache on this path falls back to a stale copy when the network
is gone — that is what keeps a device skipping sponsors and listing its library
offline — so a refresh that unlinked what it could not replace would leave a
device on a flaky connection worse off than before the button was pressed. A
failed forced fetch therefore leaves the cached copy exactly where it was and
returns the error, and lunchboxd turns that into a diagnostic.

The cooldown markers are the exception, because there the marker *is* the
staleness: an item is held back precisely by the record of its last failure.

## `yt-dlp` runs in a cgroup of its own (issue #144)

`lunchboxd` accepts a client on its management socket only from its own cgroup,
and a subprocess it spawns is a direct child, so it inherits that cgroup and
lands inside the allow-list. That is fine for a helper with fixed argv whose
output is read straight back. It is not fine for `yt-dlp`: it runs on a
background prefetch timer with no activity launched, and it parses whatever a
remote host returns.

So the two invocations that touch the network — the download in `download.rs`
and the playlist fetch in `playlist.rs` — go through
`subprocess::ytdlp_command`, which puts them in a transient scope of their own.
The `yt-dlp --version` liveness probe does not: it parses no remote input, and
it runs on every playlist fetch and diagnostics pass.

`lunchboxd` likewise injects a resolver (`set_program_resolver_fn`) so `yt-dlp`
is found in a root-owned directory rather than through `$PATH` — the scope
contains a substituted `yt-dlp`, but the `--version` liveness probe runs
unscoped, so the lookup has to be safe on its own (issue #144).

This crate does **not** build that wrapper. It is shared with the player and the
Android build, neither of which has a systemd user manager, and the probe for
whether scoping works at all lives in `lunchbox-host-linux`. Instead `lunchboxd`
injects one at startup via `set_scope_prefix_fn`. Anything that has not called
it runs `yt-dlp` bare, exactly as before — which is what every test, the player,
and Android do.

## Layout on disk

One flat directory, four kinds of file per entry:

| File | Meaning |
|---|---|
| `<key>.<ext>` | the video |
| `<key>.done` | commit sentinel; records the interest key, selector, and library position |
| `<key>.part` | an in-flight direct-HTTP download |
| `<key>.lock` | download claim |
| `<ikey>.played` | the video has been watched (keyed by *interest*, not content) |
| `<ikey>.seen` | when the item was first offered to this device |

A video counts as cached only when its `.done` sentinel exists, so a reader
never picks up a half-written file — yt-dlp in particular leaves unmerged
per-format files around mid-download.

## Two keys

Derived by `lunchbox_media_app::cache_key`, shared with the Android cache so the
two agree on what a cached file is called — and, more to the point, on not using
a hash whose output changes between Rust releases.

**Content key** — the first 128 bits of the SHA-256 of the source URL *and* the
yt-dlp format selector. It names the video, its sentinel, its part file, and its
lock.

Item ids are unique only *within* a library, and this is one directory shared by
every library on the device: two libraries that each define `intro` would
otherwise share a file, and whichever downloaded first would be served to both.
That was survivable while one process handled one library at a time. It stops
being survivable the moment lunchboxd prefetches every configured library at
once. Keying by URL also dedupes — the same video referenced from two libraries
is downloaded once and hits for both.

The selector is in there because two activities can point at one library with
different `quality` settings. Before, they shared a filename and each launch
saw the other's file as the wrong codec, deleted it, and re-downloaded — a loop
that never converged. Now the two renditions are two files.

**Interest key** — the SHA-256 of the URL alone. It names the two marker files,
`<ikey>.played` and `<ikey>.seen`, and it exists because a child who watches a
video has shown interest in the *video*, not in a particular rendition of it:
watching at 1080p protects the 480p copy too. The same goes for how old an item
is — that is a fact about the item, not about one download of it.

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

## Eviction: one score, and the grace that erodes

The scoring is `lunchbox_media_app::lru` and the markers behind it are
`lunchbox_media_app::interest` — both shared with the Android cache, so the two
front-ends spend disk the same way.

The cap is 10 GiB, or `SHEPHERD_MEDIA_VIDEO_CACHE_MAX_BYTES`. Every committed
file is scored on one time axis and the lowest score is evicted first:

```
watched    score = played_at  + watched_grace
unwatched  score = first_seen - min(ordinal, 168) * 1h
```

**Watching buys a grace, not a permanent claim.** `watched_grace` defaults to 30
days (`service.media.watched_grace_days`), and because it is a fixed head start
on a moving axis it erodes at one day per day. Inside the window nothing
speculative can touch the file. Past it, a film watched once last spring
competes on age like anything else and will lose to a video the parent added
this week.

This replaced a strict two-class ordering, where every unwatched file sorted
below every watched one. That guarantee was absolute, and absolute was too
strong in two directions: a single play protected a file forever, and once every
byte of a full cache had been watched at least once, prefetch had nothing left
it was allowed to spend and went permanently inert.

The unwatched side takes two terms because it is answering two questions.
`first_seen` is when the item entered the library, so something newly added gets
a real chance at the disk; it is a marker rather than the file's mtime because
mtime resets every time an item churns through the cache, and an item
re-downloaded twice is not a new item. `ordinal` is where the item sits in its
library, which is a proxy for how soon anyone will reach it: prefetch fills in
display order, so within one sweep the tail goes first and the head — the file a
browsing child reaches first — stays. The old model had one axis serving both,
so it inverted download time (newest first) to avoid re-downloading the head
every sweep, and in doing so made every freshly added item the first thing
thrown away.

Position deliberately cannot outweigh age: the step is an hour and it caps at
168, so a whole library spans at most a week, while the ages it competes against
run to months.

Nothing here needs a hysteresis deadband, because none of the inputs drift.
`first_seen` is write-once and `ordinal` comes from the library, so a file that
loses a comparison today loses it again tomorrow rather than trading places with
whatever replaced it. The one input that moves is `played_at`, and it moves
because somebody watched something.

`.played` is written when playback actually starts from a cached file, and by
the after-play download path (that download exists *because* the video was
watched — without the marker it would arrive scored as a guess and be the first
thing the following trim discarded, evicting itself). `.seen` is written for
every item a prefetch pass walks, including ones it does not download: an item
the cache had no room for still has to be correctly aged when room appears.
Neither is ever removed, including when the video is evicted.

### What a download may spend

One rule covers both callers — **a download may only evict what it outranks** —
and they differ only in what the download is worth:

- `queue_prefetch` scores as a guess at its library position. It recycles space
  held by other guesses and by content whose grace has expired, and is dropped
  rather than displace a file it does not outrank. If nothing is cheap enough,
  the prefetch is skipped.
- `queue_after_play` scores as a play at the current moment, which is the
  maximum, so it may displace anything — except a file watched equally recently,
  which is how it avoids evicting the file it just wrote.

Equality is not enough to displace, and that is what makes the self-eviction
case work: a download and the file it just committed score identically.

`cached_path` is a pure lookup and records nothing — enumerating the cache,
which the prefetcher does, must not make every guess look watched.

### Two processes, one policy

The grace has to be the same on both sides of the directory or the daemon and
the player would undo each other's trims. lunchboxd resolves
`service.media.watched_grace_days` once and hands it to both: to the cache its
own prefetcher builds, and to each media activity it spawns, as
`--watched-grace-days`.

### On upgrade

Files cached before the markers existed get `.seen` stamped on the first sweep
that walks them. Taking that stamp at face value would present the whole
existing cache as freshly added, so the file's own mtime is a floor on
`first_seen` — whichever is older wins. Sentinels with no recorded ordinal read
as the tail of a list nothing is walking, and pre-hash leftovers, which have no
interest key at all, read as never watched and age out on their own.
