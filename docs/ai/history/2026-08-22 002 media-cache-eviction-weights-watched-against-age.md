# Weighing watched content against fresh additions in the video cache

> Branch: `feat/media-kind` (PR #142, closes #127)
> Follows: `2026-08-22 001 media-kind-scope.md`, which introduced the two-class
> ordering this replaces.

## Prompt

After a walkthrough of the cache logic and its behaviour near capacity:

> hm that's not quite what I want here -- suggest an approach that balances the
> two. I want something that weights the relative age so that content that was
> watched once but clearly not in a while can be flushed in favor of something
> the parent had just added to the library (though IIRC we don't have a notion
> of that...)

Then: `just implement it`.

## What was wrong

`Recency` was a strict two-class ordering: every unwatched file sorted below
every watched one. The guarantee it bought — a guess can never cost the child
something they chose — was absolute, and absolute was too strong in two
directions.

1. **A single play was a permanent claim on the disk.** A film watched once, six
   months ago, outranked a video the parent added to the library yesterday, and
   would go on doing so forever.
2. **A full cache could go inert.** Once every byte had been watched at least
   once, a prefetch had nothing it was allowed to spend, and the cache stopped
   accepting new content altogether. This was known and documented at the time
   as an acceptable edge; it is the same bug as (1) seen from the other end.

There was a third problem the prompt did not name. Among unwatched files the
ordering was *inverted* download time — newest evicted first — because prefetch
walks a library in display order, so the newest arrival is the furthest down the
list and evicting the head would have the next sweep immediately re-download it.
That reasoning is sound, but it made download time serve two questions at once,
and the answer to the other one came out backwards: **the newest unwatched file
is also exactly the item a parent just added**, and it was the first thing
thrown away.

The user's aside — "IIRC we don't have a notion of that" — was correct.
`shepherd_media_core::Item` has no date field, and a library file carries no
history. "Recently added" had to become something the device *observes*.

## The model

One score on the time axis, lowest evicted first:

```
watched    score = played_at  + watched_grace
unwatched  score = first_seen - min(ordinal, RANK_CAP) * POSITION_STEP
```

**A constant additive grace is the decay the prompt asked for.** It is a fixed
head start on a moving axis, so it erodes at exactly one day per day. Watched
yesterday scores `now + 30d` and is untouchable; watched six weeks ago scores
`now - 12d` and loses to anything added in the last twelve days. The protection
is now time-bounded rather than absolute, which is the deliberate policy change
here and the one thing a parent might notice — hence
`service.media.watched_grace_days` rather than a constant.

The unwatched side splits the old conflated axis in two:

- **`first_seen`** — when the item entered the library, as far as this device
  saw. A new item deserves a chance at the disk.
- **`ordinal`** — where it sits in that library, a proxy for how soon anyone
  reaches it. This is what preserves the anti-thrash property: within one sweep
  every file shares a `first_seen`, so position decides and the tail goes first.

`POSITION_STEP` is an hour and `RANK_CAP` is 168, so a whole library spans at
most a week — position breaks ties, it never outweighs the months of age it has
to compete against. Without the cap a thousand-item playlist would spread its
tail across a year.

## Two new facts on disk

**`<ikey>.seen`** — an empty marker, keyed by interest (URL), written the first
time an item is queued and never updated. It could not live in the `.done`
sentinel, which is deleted on eviction: the whole point is that an item which
has churned through the cache twice is not a new item. It is stamped for *every*
item a prefetch pass walks, not only the ones it downloads, so an item the cache
had no room for is still correctly aged when room appears.

**`ordinal=N`** in the `.done` sentinel. The directory is shared by every
library on the device and has no idea where a file came from, let alone where in
a list, so the position has to be recorded at download time.

On upgrade, every pre-existing file gets `.seen` stamped on the first sweep that
walks it. Taking that at face value would present the entire existing cache as
freshly added and scramble the ordering, so **the file's own mtime is a floor on
`first_seen`** — whichever is older wins. Sentinels with no ordinal read as the
tail of a list nothing is walking; pre-hash leftovers, with no interest key at
all, read as never watched and age out as before.

## One eviction rule

`evict_to`/`evict_unwatched_to` collapsed into `evict_for(target, incoming)`:
**a download may only evict what it outranks**. The two callers differ only in
what the download is worth — a prefetch scores as a guess at its position, an
after-play download scores as a play right now, which is the maximum. The
"prefetch is dropped when the cache is full of watched content" behaviour falls
out of the same rule rather than being a special case, and it now resolves
itself as content ages past its grace.

Equality deliberately does not displace, which is what stops a download evicting
the file it just wrote — the two score identically.

### The margin that had to go

The first draft included a one-day hysteresis deadband, on the theory that
near-equal files would otherwise trade places every sweep. It was wrong twice
over. It swamped `POSITION_STEP` by 24×, so no guess could ever recycle another
guess from the same sweep and the cache went inert again — the exact bug being
fixed. And it was unnecessary: **none of the inputs drift.** `first_seen` is
write-once and `ordinal` comes from the library, so a file that loses a
comparison today loses it again tomorrow instead of drifting back above its
replacement. The only input that moves is `played_at`, and it moves because
somebody watched something, which is a real change of standing and should win.

## Behaviour at capacity, restated

- **Full of same-sweep guesses** — a guess further down the library is dropped
  rather than swapped for one nearer the head. A library larger than the cache
  therefore caches a stable prefix instead of rotating, which is a change from
  the old model and a better one: no churn, and the head of the list is what a
  browsing child reaches first.
- **A new arrival, wherever it sits** — outranks guesses made long enough ago,
  because the position penalty caps well below the ages it competes with. This
  is what keeps a full cache accepting content.
- **Full of recently watched content** — prefetch still skips. Unlike before,
  this state expires.
- **Full of long-ago-watched content** — prefetch now proceeds, spending the
  least recently watched file.

## Keeping the two processes agreeing

The daemon and the player write to one directory, so a disagreement about the
grace would have them undoing each other's trims. shepherdd resolves
`service.media.watched_grace_days` once and hands it to both: to the cache its
own prefetcher builds, and to each spawned media activity as
`--watched-grace-days`, via a new `SpawnOptions::media_watched_grace_days`.

A flag rather than an environment variable, and resolved by the caller rather
than read from the entry, for the same reasons `connectivity_check` already
works that way (`media_argv` passes every setting explicitly). An env var would
have meant giving the lean host adapter a dependency on `shepherd-media-cache`
just to name a constant.

## Making the two agree across a reload

Handing the grace to activities at launch introduced a divergence the first
version missed. `MediaPrefetcher` snapshotted policy at construction — the
comment said so, and the reason was sound: the engine lock must not be held
across a download. But the launch path reads `eng.policy()` live, so after a
reload that changed `watched_grace_days` a spawned player would use the new
value while the prefetcher kept the old one, and the two would spend the shared
directory by different rules until shepherdd restarted.

`[service.media]` is now cloned from the engine at the top of every sweep and
used for that sweep. The lock is held for the length of a clone, which keeps the
property the snapshot existed to protect. It also makes the other three settings
reload-responsive, and `may_sweep` now consults `prefetch`, so switching prefetch
off stops a task that is already running rather than only preventing the next
one from starting.

The target list is rebuilt from the same read, so adding, removing, disabling,
or retargeting a `media` entry lands on the next sweep as well. That forced one
structural change: `from_policy` used to return `None` when no media entry
existed, and the task was then never spawned — so a reload could never hand work
to a prefetcher that had decided at startup it had none. It is now always
constructed, and an idle sweep costs a lock, a clone, and a walk of the entry
list once an hour.

Resolution moved into a free `read_policy`, which is deliberately pure: it runs
with the engine lock held, while the yt-dlp probe it feeds spawns a process and
the library check behind it touches the disk. Those run after the lock is
dropped. The probe is also re-run only when the target set actually changes —
an hourly reminder that yt-dlp is missing is noise, but a reload that *adds* a
YouTube entry to a device without it should say so.

`warn_about_missing_ytdlp` now takes the collected `(entry id, library)` pairs
rather than the whole policy, which is what let it move outside the lock. It
still covers media entries prefetch skips, because a missing yt-dlp breaks those
when a child taps the tile, not only when this task would have downloaded them.

## Android

`lru.rs` and `interest.rs` are shared with the Android cache, so the policy had
to degrade rather than fork. That cache has no library ordinal (nothing
prefetches in display order there yet), so every file scores as the tail of a
list nothing is walking; `first_seen` falls back to the file's mtime with the
same floor rule. Its "newest unwatched evicted first" test became "oldest
arrival first", which is the correct behaviour for a cache that does not
prefetch and was only ever inherited from the shared ordering.
