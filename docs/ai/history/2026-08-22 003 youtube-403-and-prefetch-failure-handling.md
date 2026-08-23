# Every YouTube prefetch failing, and the four fixes that came out of it

> Branch: `feat/media-kind` (PR #142)
> Follows: `2026-08-22 002 media-cache-eviction-weights-watched-against-age.md`

## Prompt

While watching the cache directory on a real device with a YouTube playlist
library configured:

> hm it looks like it stopped after downloading exactly two videos and there is
> no yt-dlp process anymore

then, after the diagnosis:

> do all of the fixes, including the extractor-args

## What the journal actually said

It had not stopped. The queue drained normally: 89 items queued at 20:26:32, and
between 20:26:34 and 20:30:39 the log carries **87** `failed to cache video …
yt-dlp exited with exit status: 1`. No yt-dlp process because there was no work
left until the next hourly sweep.

The first read of this — that the two survivors were already-cached items
skipped at debug level — was wrong, and the user pushing back on it ("how did
the 2 complete then") is what produced the real answer. Inter-failure gaps are
2–4s throughout except for three outliers of 6s, 11s and 14s: long enough for an
actual download. Diffing the playlist's video ids against the 87 failures left
exactly two that had been queued and had not failed:

- `PcoAZGb4h5g` — "How People Make Crayons | **Full Episode** | Mister Rogers"
- `B7UmUX68KtE` — "Pöpcørn | Recipes with The Swedish Chef | **The Muppets**"

Both DRM-protected "full episode" uploads. Running the second with the exact
production arguments prints `Downloading 1 format(s): 18` — the legacy
progressive stream, which is precisely what `YOUTUBE_EXTRACTOR_ARGS` was added
to reach.

## Root cause

`YOUTUBE_EXTRACTOR_ARGS` was `youtube:player_client=android_vr,android`, which
*replaces* yt-dlp's default client list. When it was written, `android_vr`
served DASH without a PO token and the default clients were gating those, so it
was a strict improvement. YouTube has since started rejecting `android_vr`'s
media URLs with HTTP 403 partway through the transfer. The setting inverted:

| Video | Client path | Result |
|---|---|---|
| ordinary upload | `android_vr` → DASH | **403 Forbidden** |
| DRM "full episode" | `android` → progressive itag 18 | works |

So it preserved only the edge case it was added for and broke everything else.
Pre-existing on `main`, and shared with mpv's `ytdl_hook` and the Android stream
resolver — not something this branch introduced, but something it surfaced.

Updating yt-dlp (2026.07.04 → 2026.08.19) was worth doing and was **not** the
fix; the 403 reproduced on the new version.

## The fix: add, don't replace

`youtube:player_client=default,android`. `default` is yt-dlp's own list, which
serves working DASH for an ordinary video; `android` rides alongside it purely
for the itag 18 the defaults report as unavailable. Format selection then sorts
it out per video — a normal upload matches the DASH selector, a DRM one matches
nothing and lands on the muxed fallback, which is itag 18.

Verified on both, resolution and full download:

| Video | format selected | download |
|---|---|---|
| `6sk2j8gFdoU` (ordinary) | `137+251` | 295 MB, exit 0 |
| `B7UmUX68KtE` (DRM) | `18` | 8.45 MB, exit 0 |

The general lesson is in the constant's doc comment now: pinning a single
non-default player client bets the common case on one client's continued good
behaviour. Adding to the defaults keeps the fallback without taking that bet.

### Playback was broken too, and is fixed

The download path and mpv's `ytdl_hook` share the constant, so the streaming
path — what plays when an item is *not* cached — was checked in the headless dev
session against a fixture with `prefetch = false` and an empty cache, on an
ordinary (non-DRM) YouTube upload.

- **New value:** `STARTED_PLAYBACK item=… source=youtube`, decoded frames on the
  virtual output, audio confirmed by the user, and the cache directory still
  holding no video — so it genuinely streamed rather than quietly playing a
  cached file.
- **Old value, same fixture:** `STARTED_PLAYBACK` followed immediately by
  `ERROR item=… message=Raw(-16)` and `RETURNED_TO_MENU reason=error`.

So the same 403 that failed every prefetch also killed playback of any video
that was not already cached, which on a fresh device is all of them. mpv's
length-prefix quoting of the value (`%<len>%<value>`, so its key/value parser
does not split on the comma between clients) computes the length from the
constant, so the shorter value needed no change there.

## Three defects the incident exposed

**yt-dlp's stderr was discarded.** `download_youtube` set
`.stderr(Stdio::null())`, so 87 failures produced "exited with 1" and nothing
else, and the cause had to be found by reproducing invocations by hand.
Everything that goes wrong goes wrong *inside* yt-dlp — a 403, a format that
matches nothing, an age gate — and they all exit 1. Stderr is captured now and
its last lines go into the warning, bounded to 3 lines and 400 characters so a
library that has entirely stopped working does not put a screenful in the
journal per item.

**A failed item was retried every sweep, forever, at full speed.** Nothing
recorded that a download had failed, so a permanently broken library meant an
hourly burst of doomed fetches and 87 warnings. A `<key>.failed` marker now
records when the last attempt failed, and a speculative download inside
`RETRY_COOLDOWN` (6 hours, deliberately longer than the sweep) is skipped. It is
keyed by *content*: a rendition that will not download says nothing about the
others. An earned download ignores it entirely — someone is waiting on that one,
and a stale marker must not be why they get nothing.

This needed `remove_cached_item` to stop deleting it. That function clears
everything under a key except `.lock`, and the worker calls it right after
recording a failure to clear yt-dlp's debris — which deleted the record in the
same breath and made the cooldown a no-op. The exemption is now `is_bookkeeping`,
covering both, with a test that pins it.

**Downloads were unpaced.** 89 back-to-back yt-dlp invocations from one address.
Nothing observed was attributed to that rate — the 403s were the client setting —
so this is hygiene rather than a diagnosed fix, and it is written down that way.
Five seconds between attempts that reached the network; skipped items still
drain instantly. Configurable, and zero in tests, which poll on a deadline.

## And one unrelated bug, found in the same log

```
live playlist fetch failed: failed to parse yt-dlp output (line 47):
invalid type: null, expected a string at line 1 column 14; using stale cache
```

`YtDlpEntry.title` was `String`. yt-dlp emits `"title": null` for a video
deleted or made private since it was added to the playlist, so **one dead video
failed the entire playlist**. It survived here only because a stale cache
existed; on a fresh device that library would not have loaded at all.

`title` is `Option<String>` now, and `parse_flat_playlist` skips an entry it
cannot use — an unparseable line or one with no title — instead of aborting.
A family playlist accumulates dead videos as a matter of course, and losing the
other eighty-eight over one of them is the wrong trade. Only a playlist with
nothing usable in it is still an error. The skips are logged at debug, not warn:
videos disappearing is normal and there is nothing anyone can do about it.

Checked against the real playlist from the report: 93 lines, one null title,
92 entries parsed, where before it was a hard failure.

## On Android

Both changes are shared with `shepherd-media-android`, which is a separate
check: it neither uses mpv's `ytdl_hook` nor the host's yt-dlp, but passes
`YOUTUBE_EXTRACTOR_ARGS` to youtubedl-android's own bundled copy, refreshed from
GitHub at startup. Confirmed on a Pixel 10a — the ordinary upload and the DRM
"full episode" from the table above both resolve and play, and a live playlist
fetch exercised `parse_flat_playlist` on-device. See
`2026-08-22 004 android-on-device-validation-of-the-scored-cache.md`.


## Postscript: a false alarm the logging invited

A later test — log in, let the library download, log out mid-way, log back in —
looked like prefetch had died on the second login: no yt-dlp, no new files,
nothing after `queued media items for background download entry=youtube
items=92`.

It had not. The capture settled it in one line each way:

- 92 `.done` sentinels and 92 video files, 8.6 GiB, mtimes running 21:00→21:17
- **zero** `.failed` markers, and zero media warnings in the entire journal
- login 2 at 21:32, fifteen minutes after the last file landed

The library had finished. The second login correctly had nothing to do. (The
extra sweep at 21:09 was the `SessionEnded` resume path — an unrelated activity
stopped at 21:08:47, plus `RESUME_DELAY`.)

The first hypothesis was wrong in an instructive way: that the new `.failed`
cooldown had cascaded at logout, marking every remaining item and suppressing it
for six hours. Plausible from the code, and refuted by the absence of a single
`.failed` file. Worth writing down because the reasoning was sound and the
conclusion was not — the cache directory was the arbiter, not the argument.

What actually failed here was the log line. `queued` counted items *offered* to
`queue_prefetch`, not downloads started, and `enqueue` returns early for a cache
hit — so "items=92" printed identically whether 92 downloads were about to begin
or the library had been complete for a quarter of an hour. Every skip is
`debug!`, so INFO went silent either way. Two people read it as a failure.

`queue_prefetch` now returns a `QueueOutcome` (`Queued`, `AlreadyCached`,
`FailedRecently`, `NotCacheable`), the sweep tallies them, and the line says
what it did:

```
media prefetch sweep entry=youtube total=92 queued=0 cached=92 cooling=0
```

It is logged on every sweep now, including the empty ones, because "nothing to
do" is exactly the state that was impossible to distinguish from "broken". The
cooldown check moved into `enqueue` alongside the worker's, so a skipped item is
counted as skipped rather than as work that was started — the worker still
re-checks, since a cooldown can expire while a request sits in the queue.
