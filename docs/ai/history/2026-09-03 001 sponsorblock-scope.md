# SponsorBlock in shepherd-media (issue #159) — scope

> Status: **scoped and approved, not started.** No branch, no code. Issue #159
> was filed with a title and an empty body; the design below is a proposal, but
> the four judgement calls it turned on were answered on 2026-09-03 and are
> recorded under "Decisions" at the end. Build to those, not to the first
> proposal — where the two differ, this document has been rewritten to the
> decision and the original suggestion is noted inline.

## Prompt

> scope out #159

[Issue #159](https://git.armeafamily.com/albert/shepherd-launcher/issues/159):
*"Integrate SponsorBlock into shepherd-media for content from YouTube"* — title
only, no body, no comments.

## What this has to be

A YouTube video in a media library can open with 40 seconds of "this video is
sponsored by", and the child watching it has a controller with a seek button
they may or may not know how to use. SponsorBlock is a crowdsourced database of
those spans; integrating it means the player jumps over them.

The interesting part is not the skipping. It is that shepherd-media has two
front-ends (the Linux binary, the Android app), a cache filled by a *third*
process (shepherdd's prefetcher), an offline-first requirement, and an opt-in
resume feature that stores positions in the source timeline. Any of the three
plausible designs works for one of those and breaks another.

## The API, as verified on 2026-09-03

Checked against the live service and against yt-dlp 2026.08.19's
`postprocessor/sponsorblock.py`, which is the reference implementation this
should match:

- **Privacy endpoint** (what yt-dlp and the browser extension use by default):
  `GET https://sponsor.ajay.app/api/skipSegments/<first 4 hex chars of
  sha256(videoID)>?categories=[…]&actionTypes=[…]`. The server never learns
  which video was asked for.
- Response is an array of `{videoID, segments: […]}` for **every** video in that
  4-hex bucket; the client filters locally. A sample bucket returned **117
  videos / 180 segments / 41 KB**. Requesting one exact `videoID` instead costs
  ~1 KB but tells the server what is being watched.
- A segment is `{category, actionType, segment: [start, end], UUID,
  videoDuration, locked, votes, description}`, times in float seconds.
- Categories: `sponsor`, `selfpromo`, `interaction`, `intro`, `outro`,
  `preview`, `filler`, `music_offtopic`, `hook`, plus the non-skippable
  `poi_highlight` and `chapter`. Action types: `skip`, `mute`, `poi`, `chapter`,
  `full`.
- Responses carry an `ETag` (`"skipSegmentsHash;5f6b;YouTube;<ms>"`), so a
  refresh can be a conditional request.
- **The database is CC BY-NC-SA 4.0.** Attribution is required, and the
  *non-commercial* clause is a real constraint if this project is ever
  distributed commercially. The site offers relicensing on request.

Duration matching matters and is easy to get wrong. Segments are submitted
against a particular cut of a video; if the upload was replaced, the timestamps
point at the wrong content. yt-dlp's filter, which this should copy rather than
reinvent: drop `[0, 0]` ("full video" marker), snap a start ≤ 1s to 0, snap an
end within 1s of the duration to the duration, and accept the segment only when
`|actual_duration - videoDuration| < 1`, or `< 5` and under 5% of the segment's
own length. `videoDuration == 0` means "unknown", which is accepted.

## Three designs, and why one of them

### A. Cut the segments out at download time
`yt-dlp --sponsorblock-remove=…` on the cache download in
`crates/shepherd-media-cache/src/download.rs:210`. Zero player code.

Fails on four counts. It needs ffmpeg to rewrite the file — expensive on the
hardware this targets, and there is no ffmpeg postprocessing path at all in the
Android build (`shepherd-media-android/src/youtube.rs`: yt-dlp there is
youtubedl-android over JNI, used only for `-g`, and YouTube is never cached on
Android — `video_cache.rs`). It only helps cached items, so a cache miss or a
lost prefetch lock — both routine, see `caching_player.rs` — streams the remote
source with the ads back in, which is a behaviour a parent cannot predict. It
bakes the decision into the file: a mis-submitted segment costs real content
until someone notices and forces a re-download. And the cut file has a different
timeline from the uncut one, which silently invalidates saved resume positions
and forces the SponsorBlock config into the content key (`cache_key.rs`) or a
config change quietly serves stale cuts.

### B. Mark chapters at download time
`--sponsorblock-mark` + `--embed-chapters`, then skip chapters whose title
starts with `[SponsorBlock]`. Non-destructive, but still ffmpeg-dependent, still
cached-only, still Linux-only, and it smuggles the data through a file format
instead of keeping it as data.

### C. Fetch segments ourselves, skip at playback — **recommended**
Fetch from the hash-prefix endpoint, cache the JSON next to the video cache,
and seek past segments in the playback loop.

It is the only option that behaves identically whether the file is cached or
streaming, works on Android, survives a database correction without a
re-download, leaves the media file and the resume timeline untouched, and can be
turned off or re-scoped by config without touching the cache. It also fits the
shape this codebase already uses twice: a pure core, a platform fetch, an
on-disk cache with a TTL and a stale-on-failure fallback (`playlist.rs`,
`poster_cache.rs`).

Its cost is that the skip logic has to be written and tested, and it must be
written once, in the shared core, or the two front-ends will drift.

## Architecture

Three layers, matching how playlists and posters are already split.

**1. `shepherd-media-core::sponsorblock` — pure, no network, no I/O.**
- `Category` (with the non-skippable ones represented but never skipped),
  `ActionType`, `Segment { category, action, start, end, votes, locked }`.
- `parse_segments(json, video_id, actual_duration, enabled_categories) ->
  Vec<Segment>`: local filter of the bucket by `videoID`, yt-dlp's duration
  filter verbatim, category filter, then normalisation — drop `votes < 0`,
  prefer `locked` when two submissions of one category overlap, merge
  overlapping survivors, sort by start.
- `SegmentSkipper`: the state machine. `on_position(now) -> Option<f64>` returns
  a seek target. It must never skip backwards, must not re-fire on a segment the
  viewer deliberately seeked back into (a skipped segment is marked consumed;
  a backwards seek past its start un-consumes it), must tolerate the position
  jitter of a 60 Hz UI polling mpv, and must ignore a segment that ends within a
  second or two of the end of the video rather than seek into EOF.
- `youtube_video_id` already exists (`uri.rs:174`); this rides on it.
- Tests here are the bulk of the correctness story: table-driven over fixtures,
  no network, runs on both platforms' CI.

**2. `shepherd-media-cache::sponsorblock` — fetch + on-disk cache.**
A near-copy of `playlist.rs`, which is the right precedent: ureq (already a
dependency), `$XDG_CACHE_HOME/shepherd/media/sponsorblock/`, a `fetched_at`
stamp, `cache::Freshness`, and a stale entry served when a refresh fails so an
offline device still skips.

**Cache by hash prefix, not by video id.** One request returns the whole
bucket, so prefetching a 100-item playlist can be satisfied from far fewer
requests, and a later lookup for a video in an already-fetched bucket costs
nothing. File name `<4 hex>.json`, holding the raw bucket plus the stamp.

Suggested TTL: 24 hours, with the stale fallback. Segments churn most in the
days after an upload; a stale bucket skipping slightly less than a fresh one is
not a failure worth a network round-trip on every play.

This is also where a `--sponsorblock-api` override for a self-hosted mirror
belongs.

**3. Front-ends.**
- Linux (`crates/shepherd-media/src/ui/mod.rs:327`): the block that already
  calls `tracker.progress(position, duration, now)` every frame while `Playing`
  is exactly where `skipper.on_position(pos)` goes, with the returned target
  handed to `self.session.seek_absolute(target)`. The fetch happens off the UI
  thread, keyed on the item starting (the same transition at `:300` that
  notifies the resume tracker), so a slow lookup delays skipping and nothing
  else.
- A "Skipped sponsor" toast in `shepherd-media-ui`, plain and brief — the docs'
  non-features section rules out animated affordances, and this should stay on
  the right side of that line.
- Android (`shepherd-media-android/src/playback.rs`): the same `SegmentSkipper`
  against `dyn PlayerHandle`, fetching with the ureq it already depends on.
  Ships after the Linux side works; the core module is what keeps them equal.

**Prefetch (`crates/shepherdd/src/media.rs:421`).** When the sweep queues a
YouTube item, also warm its bucket. Without this, a device that prefetched a
library while online skips nothing when it plays it offline — which is the exact
scenario the prefetcher exists for. It is a cheap addition to a pass that is
already walking every item in display order.

## Config surface

Mirroring `watched_grace_days`, which is already threaded end to end and is the
model to copy:

```toml
[service.media.sponsorblock]
# enabled = false                    # the whole feature; off unless a parent asks
# categories = ["sponsor", "selfpromo", "interaction", "intro", "outro"]
# api = "https://sponsor.ajay.app"   # a self-hosted mirror goes here
```

with a per-entry override on the media entry kind (`sponsorblock = false`, or a
category list) beside the existing `quality` / `resume` / `prefetch`.

Threading: `RawMediaServiceConfig` (`schema.rs:714`) → `MediaServiceConfig`
(`policy.rs:249`) → `shepherd-host-linux/src/adapter.rs:171` pushes
`--sponsorblock-categories sponsor,selfpromo,interaction,intro,outro` (the flag
absent entirely when the feature is off) → `shepherd-media/src/cli.rs`. Plus
`config.example.toml` and the config-editor schema, which is generated.

**The feature is off by default** (decision 1). It contacts a third-party
service, and in a product that promises no telemetry, nothing reaches a new host
because a default said so. A parent turns it on; nothing about the build or the
first launch does.

**Once on, the default categories are `sponsor`, `selfpromo`, `interaction`,
`intro` and `outro`** (decision 2), each individually configurable. The
distinction the two-tier default draws is between spans that are reliably not
the video and spans that are a judgement call: `preview`, `filler` and
`music_offtopic` stay opt-in, because an aggressive submission in those
categories can cut real content and make a video feel broken. A parent who went
looking for this feature wants intros and end cards gone, so they are in the
default set rather than a second thing to discover — but they remain separable
for children's content where a long animated intro is arguably the point.

`poi_highlight` and `chapter` are never skippable, and the `mute` and `full`
action types are ignored in v1 — a `mute` action is a volume change rather than a
seek, and worth doing only if anyone asks.

## Privacy, and what leaves the device

Worth being deliberate about, in a product whose whole premise is a controlled
environment for a child:

- Always the hash-prefix endpoint, never `?videoID=`. The service sees a 4-hex
  bucket and cannot tell which of ~100 videos was played. This costs ~40 KB a
  request instead of ~1 KB, and the bucket cache is what makes that fine.
- No submissions, no votes, no user ID, no `X-Client-Name` beyond a version
  string. shepherd-media consumes the database and never writes to it.
- One new outbound host, reached by shepherdd during prefetch and by the player
  at play time. Both already make HTTP requests (posters, the connectivity
  check), so this adds a destination, not a capability.
- `enabled = false` — the default — must mean *no request is ever made*: not a
  liveness probe, not a warm-up at launch, nothing. A device nobody configured
  for this must be indistinguishable on the wire from one built before the
  feature existed, and there should be a test that asserts it rather than a
  comment that claims it. The docs should say so plainly next to the
  third-party-host disclosure.
- **Caveat for firewalled entries:** `[entries.firewall]` is IP/CIDR-based
  (`schema.rs:232` — systemd `IPAddressAllow=`, no hostname matching), and
  `sponsor.ajay.app` is behind Cloudflare with rotating addresses. An entry with
  a restrictive firewall cannot allowlist it by name; it is in the same bucket
  as the googlevideo CDNs, which the same constraint already applies to.

## Phases

| # | Work | Files | Rough size |
|---|---|---|---|
| 0 | Pure core: types, parse/filter, `SegmentSkipper`, fixtures + tests | `shepherd-media-core/src/sponsorblock.rs`, `lib.rs`, `tests/fixtures/` | ~350 lines + tests |
| 1 | Fetch + bucket cache with TTL and stale fallback | `shepherd-media-cache/src/sponsorblock.rs`, `lib.rs` | ~250 lines, modelled on `playlist.rs` |
| 2 | Linux wiring: CLI flag, off-thread fetch, skip in the frame loop, toast | `shepherd-media/src/cli.rs`, `src/ui/mod.rs`, `shepherd-media-ui/` | ~150 lines |
| 3 | Config end to end + example + docs | `shepherd-config/{schema,policy,validation}.rs`, `shepherd-host-linux/src/adapter.rs`, `config.example.toml`, `docs/shepherd-media.md` | ~200 lines, mostly plumbing |
| 4 | Prefetch warms the bucket for prefetched YouTube items | `shepherdd/src/media.rs` | ~40 lines |
| 5 | Android parity | `shepherd-media-android/src/{playback,sponsorblock}.rs` | ~150 lines |

**All six phases are in scope for #159** (decision 4): the issue closes when both
front-ends skip, not when the Linux one does. Phases 0–3 are still the natural
first landing — a Linux device skipping sponsors, configurably — but 4 (offline)
and 5 (Android) are commitments, not follow-ups. That ordering is what the shared
core in phase 0 is for; it is also what makes phase 5 small, and it is worth
resisting any shortcut in phases 1–3 that moves logic out of the core and into
the Linux binary, because phase 5 pays for it twice.

## Verification

- Unit tests carry the logic: bucket filtering, the duration filter's boundary
  cases (the `< 1` / `< 5 and < 5%` rule), overlap merging, the no-backwards-skip
  and seek-back-un-consumes rules, and a segment that runs to EOF.
- Cache tests over a temp dir: fresh hit, stale-on-failure fallback, unparseable
  file treated as a miss — the same shape `playlist.rs` is already tested in.
- End to end through the `headless-dev` skill: a one-item library pointing at a
  video with a known `sponsor` segment, played headless, screenshotted before
  and after the boundary. Also the negative case — `enabled = false` makes no
  request — which is worth asserting rather than assuming.
- Manual: a video whose upload was replaced, to confirm the duration filter
  declines to skip rather than skipping into the wrong place.

## Non-features (for the docs' existing list)

- No submitting or voting on segments. Read-only client.
- No per-segment UI, no "unskip" button, no category picker in the player — the
  categories are a parent's config decision, not a runtime one.
- No `mute` or `full` action support in v1.
- No skipping for non-YouTube sources. The database is YouTube-only in practice,
  and every other source in a library is a file someone chose deliberately.

## Licensing

The database is CC BY-NC-SA 4.0; the code here is GPL-3.0 (`Cargo.toml:82`).
Those two do not have to be reconciled, because nothing is combined: no segment
data is vendored into the tree or shipped in a release. It is fetched at runtime
and cached under `$XDG_CACHE_HOME` on the device that fetched it, the same
relationship the poster and playlist caches already have with their upstreams.

So the NC clause binds how the project is *used and distributed*, not how it is
licensed — and the project is non-commercial (decision 3), which satisfies it.
Worth writing down rather than leaving implicit: it is a constraint any future
commercial distribution would inherit, and the reason for it would not be
obvious from the source.

Attribution is required and is a phase 3 deliverable: a credit line naming
SponsorBlock, linking the database, and naming the licence, in
`docs/shepherd-media.md` beside the feature's documentation.

## Decisions

Answered 2026-09-03, in reply to this scope:

1. **Default off.** The feature as a whole ships disabled; a parent opts in.
2. **`intro` / `outro` are configurable, and default on once the feature is
   enabled** — along with `sponsor`, `selfpromo` and `interaction`. `preview`,
   `filler` and `music_offtopic` remain opt-in.
3. **The project as a whole is non-commercial**, so the NC clause is satisfied.
   See "Licensing" above.
4. **Android parity is in scope for #159.** Phase 5 blocks the issue rather than
   shipping behind it.
