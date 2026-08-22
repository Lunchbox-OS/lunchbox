# shepherd-media

`shepherd-media` is a small standalone launcher for media libraries: a
declarative `.toml` file lists the items, and `shepherd-media` either plays
one of them directly via libmpv or opens a poster grid for the user to pick.
It is designed to be invoked by `shepherdd` as an activity, the same way
TuxMath or ScummVM are.

The implementation lives in three crates:

- `shepherd-media-core` — platform-agnostic library (parsing, source
  resolution, session state machine, stdout protocol).
- `shepherd-media-cache` — the on-disk video cache: keying, the download
  worker, and LRU eviction. Separate because shepherdd shares it, and a daemon
  must not link libmpv or egui to prefetch (issue #127).
- `shepherd-media` — Linux binary (`clap` CLI, libmpv via `libmpv2`,
  egui-based browse UI, async poster prefetch).

## Installation requirements

In addition to the standard `shepherd-launcher` build dependencies:

- `libmpv-dev` at build time (provides the libmpv2 client headers).
- `mpv` and `yt-dlp` at runtime. `yt-dlp` is needed only if you reference
  YouTube URLs — `shepherd-media` will start without it, but YouTube playback
  will fail if `yt-dlp` isn't on `PATH`.
- A **VA-API driver** at runtime, for hardware video decoding. `mpv` does not
  pull one in, and without it every frame is decoded on the CPU. Which driver is
  the right one depends on the GPU, so `shepherd-admin va-api install` picks it
  from the detected hardware (`va-api detect` shows what it found).
  `shepherd deps install run` and `shepherd-admin media-deps install` both
  include it; see [INSTALL.md](./INSTALL.md#hardware-video-decoding) for how to
  check it worked.

### Codec selection

YouTube's best rendition at a given resolution is usually VP9 or AV1, and the
fixed-function decoders in older GPUs cover neither. `shepherd-media` therefore
asks yt-dlp for H.264 first (`bv*[vcodec^=avc1]…`) and only falls back to other
codecs when an upload has no H.264 rendition. On an Intel HD 4000 that is the
difference between roughly 61% and 13% of a CPU core for 1080p30.

Videos already in the local cache were downloaded under whichever selector was
in force at the time; `shepherd-media` keeps playing them, and replaces them the
next time it queues that item for download.

## Video cache

Remote sources — YouTube URLs and plain HTTP files — are downloaded to
`$XDG_CACHE_HOME/shepherd/media/videos/`, so a later play comes off local disk
and the item stays watchable offline. Browse mode queues every remote item in
the library speculatively at launch, and an item watched to the end is queued
after playback.

shepherdd also fills this cache in the background, so a library is ready before
anyone opens it — see [Background prefetch](#background-prefetch).

The cache is capped at 10 GiB, overridable with
`SHEPHERD_MEDIA_VIDEO_CACHE_MAX_BYTES` (a byte count).

When it is full, every file is scored and the lowest goes first. **Watching
something protects it for `service.media.watched_grace_days` (30 by default),
not forever.** Inside that window a speculative download can never cost the
child a video they chose; past it, the file competes on age like anything else,
so a film watched once months ago will eventually yield to a video added to the
library this week. That expiry is also what keeps a full cache from freezing:
without it, a cache whose every byte had been watched at least once had nothing
prefetch was allowed to spend and stopped taking new content altogether.

Among files nobody has watched, the ordering is by when the item entered the
library — an item the parent just added outranks one that has been sitting there
unwatched since spring — with the item's position in its library breaking ties,
so within one prefetch sweep the tail of the list goes before the head. Position
cannot outweigh age: a whole library spans at most a week of the scale, while
the ages it competes against run to months.

Among watched files, the least recently played goes first.

Raise `watched_grace_days` for a household that goes offline for long stretches
and wants what it has watched to stay put; set it to 0 to drop the protection
entirely and order purely by age.

Files are named after a hash of their source URL *and* the quality selector, not
the library item id. Item ids are unique only within a library and one directory
serves every library on the device; including the selector means two activities
over one library at different qualities cache side by side instead of deleting
each other's downloads. Deleting the directory is always safe.

## Background prefetch

shepherdd downloads the remote items of every `media` entry ahead of time, so
the first open plays from disk rather than buffering and the library keeps
working offline. It is on by default whenever a media entry exists.

It holds off while:

- **an activity is running** — a download competing with a game, or with the
  video being watched right now, spends the child's CPU and bandwidth on
  content nobody has asked for. Set
  `service.media.prefetch_while_session_active = true` to allow it anyway;
- **the internet is down**, per the connectivity checks shepherdd already runs;
- **the disk is nearly full** — below `service.media.free_space_floor_bytes`
  (2 GiB by default) it logs a warning and stops. The cache cap bounds the
  cache, not the volume it sits on;
- **there is nothing in the cache cheap enough to replace** — a guess is dropped
  rather than displace a file it does not outrank.

Availability windows are deliberately ignored: an activity outside its window
today is exactly the one worth having ready for tomorrow. An entry that is
`disabled` outright is skipped.

To exclude one library, set `prefetch = false` under its `[entries.kind]` — a
24/7 live stream is the obvious case, since it has no end to download. To turn
the whole thing off, set `service.media.prefetch = false`.

### What a config reload reaches

Policy is re-read before every sweep, so a reload takes effect within the hour
without a restart. That covers both halves:

- **`[service.media]`** — `prefetch`, `prefetch_while_session_active`,
  `free_space_floor_bytes`, and `watched_grace_days`. The grace in particular
  has to work this way: the launch path hands each spawned activity the
  *current* value, so a prefetcher still running on the value from startup would
  value the shared cache directory differently from the player writing to it.
- **The set of libraries** — adding, removing, disabling, or retargeting a
  `media` entry, and the per-entry `prefetch = false` opt-out. This is also why
  the task is started even on a device with no media entries at all: a
  prefetcher that only existed when the startup policy had work for it could
  never be handed any by a reload.

The *contents* of an already-configured library are read fresh on every sweep
too, so a video the parent adds to a library file is picked up within the hour;
a playlist URL is refetched when its metadata cache expires, every 6 hours.

Nothing here reaches back and deletes what a removed entry had already cached.
Those files stop being refreshed and age out of the cache on their own, which is
also what happens if the entry is added back a week later.

Prefetch order follows the library's own order, which is what browse shows.

If any media activity references YouTube and `yt-dlp` is not installed,
shepherdd logs a warning at startup naming the entries — otherwise the failure
only appears when a child taps a tile and the activity dies.

The implementation, including how a running `shepherd-media` and a prefetching
shepherdd stay off each other's downloads, is documented in
[`crates/shepherd-media-cache/README.md`](../crates/shepherd-media-cache/README.md).

## Authoring a library file

A starter library is checked in at the repo root as
[`movies-library.example.toml`](../movies-library.example.toml). It defines
the items referenced from `config.example.toml` (Big Buck Bunny, Sintel,
Tears of Steel, and a Lofi Beats YouTube stream) and is the recommended
starting point — copy it to `~/.config/shepherd/movies.toml` and update the
`uri` paths for your own files.

A library file is TOML with the schema below. Save it anywhere readable by
the user shepherdd runs as; relative poster paths are resolved against the
library file's directory.

### M3U / M3U8 playlists

`shepherd-media` also accepts `.m3u` and `.m3u8` playlist files anywhere a
library path is required (CLI `--library`, the entries in `config.toml`,
etc.). The dispatch is by file extension; the rest of the pipeline doesn't
care which format the file was authored in.

```m3u
#EXTM3U
#EXTINF:596,Big Buck Bunny
file:///srv/media/big-buck-bunny.mp4
#EXTINF:-1,Lofi Beats (live)
https://www.youtube.com/watch?v=jfKfPfyJRdk
```

Notes:

- `library_id` and `title` are derived from the playlist filename
  (`my-list.m3u` → `library_id = "my-list"`, `title = "my-list"`).
- Item IDs are auto-generated as `track-001`, `track-002`, … so you can
  pass them directly to `--item` in direct-play mode.
- `#EXTINF:<seconds>,<title>` immediately preceding an entry sets that
  entry's title and (for non-negative durations) `duration_seconds`. All
  other `#`-prefixed lines are ignored.
- Relative paths are resolved against the playlist file's directory.
  Plain absolute paths (`/srv/media/foo.mp4`) are wrapped as `file://`
  URIs automatically.
- The same DRM/subscription rejection list applies: a Netflix URL inside
  a playlist fails validation, citing the line number.
- Note that `.m3u8` is also the extension HLS uses for stream manifests;
  inside a TOML library a `.m3u8` URI classifies as `direct-http` and is
  played directly. Only the top-level file passed via `--library` is ever
  interpreted as a playlist.

### YouTube playlist URLs

`shepherd-media` also accepts a YouTube playlist URL anywhere a library path
is required. The URL is detected by the presence of a `list=…` query
parameter:

- `https://www.youtube.com/playlist?list=PL…`
- `https://www.youtube.com/watch?v=…&list=PL…`
- `https://youtu.be/…?list=PL…`

`yt-dlp` is invoked once at startup (in `--flat-playlist` mode) to fetch the
title, video IDs, durations, and thumbnails. Results are cached under
`$XDG_CACHE_HOME/shepherd/media/playlists/<list-id>.json` for 6 hours, so
subsequent launches don't re-hit YouTube. A stale or missing cache
transparently falls back to a fresh fetch.

Notes:

- `library_id` is derived from the `list=` value (e.g. `PLtest` →
  `library_id = "pltest"`); `title` is whatever yt-dlp reports for the
  playlist.
- Item IDs are the YouTube video IDs, lowercased with `_` replaced by `-`
  (e.g. `YE7VzlLtp-4` → `ye7vzlltp-4`), so you can pass them straight to
  `--item` in direct-play mode.
- Posters use the per-video thumbnail when yt-dlp returns one, otherwise
  `https://i.ytimg.com/vi/<id>/hqdefault.jpg` is derived from the video ID.
- All items get a single `platforms = ["*"]` source pointing at the
  canonical `https://www.youtube.com/watch?v=<id>` URL.
- `yt-dlp` is a required runtime dependency for this path; without it,
  startup fails with an actionable error rather than partial results.

#### Recommended options

`yt-dlp` returns items in the playlist owner's order. For a hand-curated
playlist that is usually what you want; for a channel's "uploads"
playlist (`UU…`), which YouTube returns newest-first, add `--reverse` to
get chronological viewing.

Every source is a YouTube URL, so when the network is down the entire
grid is unreachable. Pass `--connectivity-check <url>` so the grid empties
itself out gracefully on a network drop instead of failing on the first
click. Any reachable HTTPS target works (or a `tcp://host:port` probe);
forwarding shepherdd's own `internet.check` value is the easiest choice.

```
shepherd-media browse \
    --library 'https://www.youtube.com/playlist?list=UU...' \
    --connectivity-check https://www.google.com \
    --reverse
```

### TOML libraries

If you need stable item IDs, posters, per-platform fallback sources, or
multiple sources per item, use a TOML library.

```toml
schema_version = 1
library_id = "kids-movies"
title = "Movies"

[[items]]
id = "big-buck-bunny"
title = "Big Buck Bunny"
kind = "video"             # "video" or "audio"
poster = "posters/bbb.jpg" # optional; relative path or http(s) URL
duration_seconds = 596     # optional; informational only

[[items.sources]]
platforms = ["linux"]
uri = "file:///srv/media/big_buck_bunny_720p_h264.mov"

[[items]]
id = "lofi-beats"
title = "Lofi Beats"
kind = "audio"

[[items.sources]]
platforms = ["*"]
uri = "https://www.youtube.com/watch?v=jfKfPfyJRdk"
```

### Field reference

| Field | Required | Notes |
|-------|:---:|---|
| `schema_version` | yes | Must equal `1`. |
| `library_id` | yes | `[a-z0-9-]+`, 1–64 chars. |
| `title` | yes | 1–128 chars. Used in the browse-UI heading. |
| `items[].id` | yes | `[a-z0-9-]+`, 1–64 chars. Unique within the file. |
| `items[].title` | yes | 1–256 chars. |
| `items[].kind` | yes | `"video"` or `"audio"`. |
| `items[].category` | no | Free-form tag, 1–64 chars. Currently informational. |
| `items[].poster` | no | Relative path, absolute path, or `http(s)://` URL. |
| `items[].duration_seconds` | no | Not enforced; reserved for future UI use. |
| `items[].sources` | yes | At least one source. First match wins per platform. |
| `sources[].platforms` | yes | Array of `"linux"`, `"android"`, or `"*"`. |
| `sources[].uri` | yes | See [URI classification](#uri-classification). |
| `sources[].player_hint` | no | Currently must be `"mpv"` if set. |

Unknown keys are an **error**, not a warning — typos like `titel` will
fail validation.

### URI classification

URIs are classified at parse time:

- `file://` — absolute paths only. `shepherd-media` does not check existence;
  mpv reports the failure if the file is missing.
- `http(s)://` ending in `.mp4`/`.mkv`/`.webm`/`.mov`/`.m4v`/`.mp3`/`.flac`/
  `.opus`/`.ogg`/`.m4a`/`.wav`/`.m3u8`/`.mpd` — direct HTTP stream.
- `http(s)://` on a YouTube host (`youtube.com`, `m.youtube.com`, `youtu.be`,
  `youtube-nocookie.com`) — handed to mpv with `ytdl=yes` so yt-dlp picks
  the right format.
- Anything else `http(s)://` — passed to mpv unchanged with a warning.

#### DRM and subscription services

The following hosts are **rejected at validation time**:

`netflix.com`, `disneyplus.com`, `hulu.com`, `max.com`/`hbomax.com`,
Amazon Prime Video URLs (`/gp/video`, `/Prime-Video`), Apple TV+/Music,
`peacocktv.com`, `paramountplus.com`, `spotify.com`.

URI schemes `widevine:`, `playready:`, and `fairplay:` are also rejected.

DRM-protected playback is intentionally out of scope — see issues
[#2](https://git.armeafamily.com/albert/shepherd-launcher/issues/2) and
[#10](https://git.armeafamily.com/albert/shepherd-launcher/issues/10).

## CLI

```
shepherd-media validate <library-or-url>
    Parse and validate the library file (`.toml`, `.m3u`, `.m3u8`) or
    YouTube playlist URL. Exit 0 on success, 1 on error.

shepherd-media play --library <library-or-url> --item <item-id>
    Direct-play mode. Resolve the item, hand off to mpv, exit when playback
    ends. Used when an item is registered as its own shepherdd activity.

shepherd-media browse --library <library-or-url>
    Open the egui poster grid. Each playback is a state-machine transition;
    the process keeps running until the user exits or shepherdd sends
    SIGTERM.
```

Global flags:

```
--log-level <error|warn|info|debug|trace>                default: info
--no-protocol                                            suppress stdout protocol
--quality <best|1080p|720p|480p>                         default: 1080p
--sort-by <library|title|id|kind|category|duration>      default: library
--reverse                                                reverse item order
--resume                                                 remember playback positions
--connectivity-check <url>                               browse-mode online probe
```

`--sort-by library` preserves the order from the library file or playlist.
The sort is stable, so library order breaks ties for any other key. Items
missing the chosen field (no `category` or `duration_seconds`) sort to the
end in ascending order. `--reverse` is applied after sorting; with the
default `--sort-by library` it just flips the file order.

`--resume` is off by default and described in
[Resuming playback](#resuming-playback).

`--connectivity-check <url>` is honored only by `browse`: the URL is
probed every 10 seconds and items without a local source are hidden when
the probe fails. `validate` and `play` accept the flag for invocation
symmetry but ignore it. Accepts the same format as shepherdd's
`internet.check` (e.g. `https://www.google.com` or `tcp://8.8.8.8:53`).

Exit codes:

| Code | Meaning |
|---:|---|
| 0 | success |
| 1 | library validation error |
| 2 | invocation error (bad flags, missing item, no source for platform) |
| 3 | unrecoverable player error |
| 4 | signal-driven shutdown (SIGTERM/SIGINT) |

## Stdout protocol

While `shepherd-media` runs, it writes one event per line to stdout
(unless `--no-protocol` is passed). Each line is `EVENT key=value [key=value ...]`,
with values percent-encoded if they contain spaces or `=`.

| Event | Fields | When |
|---|---|---|
| `READY` | `library_id`, `item_count` | Once at startup. |
| `STARTED_PLAYBACK` | `item`, `kind`, `source` (`local`/`direct-http`/`youtube`/`unknown`) | mpv has accepted the load command. |
| `RETURNED_TO_MENU` | `item`, `reason` (`eof`/`closed`/`user`/`error`) | Playback has ended. |
| `WARNING` | `item`, `reason` | e.g. unknown item id, or no source for platform. |
| `ERROR` | `item`, `message` | Player-level error. |
| `EXIT` | `reason` (`user`/`signal`/`crash`) | Final line before process exit. |

Stderr is for human-readable logging via `tracing`; it is not part of the
protocol.

`shepherdd` can read this stream to drive playback-only time accounting:
start the clock on `STARTED_PLAYBACK`, pause on `RETURNED_TO_MENU`. The
shepherdd-side support for that is tracked separately; for now `shepherd-media`
emits the protocol unconditionally and works fine when shepherdd ignores it.

## shepherdd integration

Media activities use `type = "media"` (issue #127). shepherdd builds the
`shepherd-media` command line itself from the entry, so the flags above do not
have to be restated as a `Process` argv, and a mistake — a `mode = "play"` with
no `item`, a quality that isn't a preset — is caught by
`shepherd-admin config validate` instead of on the child's screen.

| Field | Default | Meaning |
|---|---|---|
| `library` | *required* | Path to a `.toml`/`.m3u`/`.m3u8`, or a YouTube playlist URL. `~` is expanded for paths. |
| `mode` | `"browse"` | `"browse"` opens the poster grid; `"play"` plays one item end to end. |
| `item` | — | The item to play. Required by `mode = "play"`, rejected otherwise. |
| `quality` | `"1080p"` | `best`, `1080p`, `720p`, `480p` — see [Codec selection](#codec-selection). |
| `sort_by` | `"library"` | `library`, `title`, `id`, `kind`, `category`, `duration`. |
| `reverse` | `false` | Reverse the final order; combines with `sort_by`. |
| `resume` | `false` | Remember playback positions — see [Resuming playback](#resuming-playback). |
| `prefetch` | `service.media.prefetch` | Let shepherdd download this library ahead of time — see [Background prefetch](#background-prefetch). |

There is deliberately no field for `--connectivity-check`: shepherdd already
knows the check from the entry's `[entries.internet]` block, or
`[service.internet]` when the entry sets none, and hands it to the activity
automatically. Set `forward_check = false` under `[entries.internet]` to launch
without one. `--log-level` and `--no-protocol` are not exposed either; they are
debugging flags, and shepherdd picks them.

An activity that needs a flag this kind doesn't expose can still be spelled out
as `type = "process"` with `command = "shepherd-media"`; nothing about that
path changed.

### Direct-play activity (single item)

```toml
[[entries]]
id = "big-buck-bunny"
label = "Big Buck Bunny"
icon = "media-video"

[entries.kind]
type = "media"
library = "/etc/shepherd/movies.toml"
mode = "play"
item = "big-buck-bunny"
```

shepherdd supervises this exactly like any other activity: the session ends
when the process exits.

### Browse-mode activity (whole library)

```toml
[[entries]]
id = "movies-library"
label = "Movies"
icon = "folder-videos"

[entries.kind]
type = "media"
library = "/etc/shepherd/movies.toml"
resume = true
```

Once shepherdd grows protocol-reader support, the browse activity can opt
into playback-only time accounting. Until then, browse-mode time counts as
"in the activity" for as long as the process runs.

### Browse a YouTube playlist

A YouTube playlist URL is accepted wherever a library path is — no TOML
file required.

```toml
[[entries]]
id = "channel-uploads"
label = "My Channel"
icon = "video-x-generic"

[entries.kind]
type = "media"
library = "https://www.youtube.com/playlist?list=UU..."
reverse = true

# Optional: without it the service-wide check is forwarded instead.
[entries.internet]
check = "https://www.google.com"
```

See [YouTube playlist URLs](#youtube-playlist-urls) for the caching
behavior and why `reverse` and a connectivity check are recommended.

### Icons

An entry with no `icon` gets one from its mode: `folder-videos` for `browse`,
`video-x-generic` for `play`.

## Playback UI

`shepherd-media` embeds mpv into its egui shell rather than letting mpv
spawn its own window. The same fullscreen surface hosts the poster grid
in browse mode and the video + a touch- and controller-friendly control
overlay during playback.

Controls auto-hide after ~3 seconds of input silence. Any pointer
activity (mouse or touch), key press, or gamepad input summons them
back. While paused, the overlay stays visible.

| Action            | Touch / Mouse                  | Keyboard            | Gamepad                         |
|-------------------|--------------------------------|---------------------|---------------------------------|
| Play / Pause      | Tap the play button            | `Space`, `K`        | A (south)                       |
| Back to grid      | Tap the back button            | `Esc`, `Backspace`  | B (east), Start, Select         |
| Skip −10 seconds  | Tap the « 10s button           | `←`, `J`            | D-pad left, LT                  |
| Skip +10 seconds  | Tap the 10s » button           | `→`, `L`            | D-pad right, RT                 |
| Scrub             | Drag the scrubber              | —                   | —                               |

Volume is intentionally not bound in the playback overlay — `shepherd-hud`
already exposes global volume controls that work the same everywhere.

In direct-play mode the same UI opens straight into playback and the
process exits once the item finishes; the grid is never shown.

## Resuming playback

Off by default. Pass `--resume` (Linux) or turn on **Resume playback** for a
library in the Android app's settings, and `shepherd-media` remembers, per
library:

- where each item was left off, and
- which item was watched most recently.

With it on:

- **Re-opening an item resumes it.** The position is handed to mpv with the
  file, so nothing before it is decoded or shown. This applies wherever
  playback starts — a tile in the grid, the card below, or `play --item`.
- **Re-opening the library offers to continue.** Browse mode opens with a
  "Continue watching" card naming the last item and where it stopped; the
  choices are **Resume** and **Library** (dismiss and browse as usual). Enter /
  A resumes, Escape / B / BACK dismisses. The card is skipped when the library
  has never been watched, or when that item is no longer in it.
- **A finished item is forgotten.** A stop within 30 seconds of the end (and a
  stop within the first 20 seconds) clears the position, so the next play starts
  from the beginning rather than at the credits.
- **A restart after a stream error comes back to where it dropped**, instead of
  to the opening titles.

Positions are written while an item plays (at most every 10 s) and when playback
ends, including when shepherdd stops the activity with SIGTERM — a time limit
expiring mid-film does not lose the place.

State lives outside the caches, one file per library, at
`$XDG_STATE_HOME/shepherd/media/resume/<library_id>.toml` (falling back to
`~/.local/state`) on Linux and in app-private storage on Android. Deleting a
file just forgets that library's positions. Only item ids, second offsets, and
durations are stored — no timestamps, no history of what was watched when. With
the option off nothing is recorded and no file is written.

## Non-features

These are deliberately not implemented:

- Playlists, queues, autoplay, "watch next", recommendations, viewing history.
  (Opt-in resume keeps a position per item and the id of the last item watched —
  see [Resuming playback](#resuming-playback) — and nothing else. It is not a
  log of what was watched when, and there is no UI that lists it.)
- Library-file hot-reload. Edit the file, restart the activity.
- Subscription-service DRM playback.
- Any animated/celebratory UI affordances (the touch overlay is plain;
  the scrubber doesn't bounce, no on-completion confetti).
