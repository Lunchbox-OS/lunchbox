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

## Authoring a library file

A library file is TOML with the schema below. Save it anywhere readable by
the user shepherdd runs as; relative poster paths are resolved against the
library file's directory.

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
shepherd-media validate <library.toml>
    Parse and validate the library file. Exit 0 on success, 1 on error.

shepherd-media play --library <library.toml> --item <item-id>
    Direct-play mode. Resolve the item, hand off to mpv, exit when playback
    ends. Used when an item is registered as its own shepherdd activity.

shepherd-media browse --library <library.toml>
    Open the egui poster grid. Each playback is a state-machine transition;
    the process keeps running until the user exits or shepherdd sends
    SIGTERM.
```

Global flags:

```
--log-level <error|warn|info|debug|trace>   default: info
--no-protocol                               suppress stdout protocol
```

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

## Non-features

These are deliberately not implemented:

- Playlists, queues, autoplay, "watch next", recommendations, history.
- Library-file hot-reload. Edit the file, restart the activity.
- Mouse support in the browse UI (kiosk inputs are keyboard and gamepad).
- Subscription-service DRM playback.
- Any animated/celebratory UI affordances.
