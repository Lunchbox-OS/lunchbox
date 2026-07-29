# shepherd-media

`shepherd-media` is a small standalone launcher for media libraries: a
declarative `.toml` file lists the items, and `shepherd-media` either plays
one of them directly via libmpv or opens a poster grid for the user to pick.
It is designed to be invoked by `shepherdd` as an activity, the same way
TuxMath or ScummVM are.

The implementation lives in two crates:

- `shepherd-media-core` — platform-agnostic library (parsing, source
  resolution, session state machine, stdout protocol).
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
--connectivity-check <url>                               browse-mode online probe
```

`--sort-by library` preserves the order from the library file or playlist.
The sort is stable, so library order breaks ties for any other key. Items
missing the chosen field (no `category` or `duration_seconds`) sort to the
end in ascending order. `--reverse` is applied after sorting; with the
default `--sort-by library` it just flips the file order.

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

### Direct-play activity (single item)

```toml
[[entries]]
id = "big-buck-bunny"
label = "Big Buck Bunny"
icon = "media-video"

[entries.kind]
type = "process"
program = "shepherd-media"
args = ["play", "--library", "/etc/shepherd/movies.toml", "--item", "big-buck-bunny"]
```

shepherdd treats this exactly like any other process activity.

### Browse-mode activity (whole library)

```toml
[[entries]]
id = "movies-library"
label = "Movies"
icon = "folder-videos"

[entries.kind]
type = "process"
program = "shepherd-media"
args = ["browse", "--library", "/etc/shepherd/movies.toml"]
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
type = "process"
program = "shepherd-media"
args = [
    "browse",
    "--library", "https://www.youtube.com/playlist?list=UU...",
    "--connectivity-check", "https://www.google.com",
    "--reverse",
]
```

See [YouTube playlist URLs](#youtube-playlist-urls) for the caching
behavior and why `--reverse` and `--connectivity-check` are recommended.

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

## Non-features

These are deliberately not implemented:

- Playlists, queues, autoplay, "watch next", recommendations, history.
- Library-file hot-reload. Edit the file, restart the activity.
- Subscription-service DRM playback.
- Any animated/celebratory UI affordances (the touch overlay is plain;
  the scrubber doesn't bounce, no on-completion confetti).
