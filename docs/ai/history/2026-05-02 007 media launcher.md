# shepherd-media: Implementation Specification

## 0. Purpose and scope

`shepherd-media` is a standalone media-library launcher that resolves a declarative
library file into an mpv-launched playback session. It exists primarily to be
called by `shepherd-launcher` on Linux, but is designed to be reusable as a
sideloaded Android TV / Fire TV app later.

This document specifies the **Linux implementation** in full. Decisions that exist
specifically to keep the Android port feasible are flagged with
`[android-portability]`. An agent implementing this spec should treat those flags
as constraints, not as work items.

### In scope (this document)

- A new Cargo workspace member `shepherd-media-core` containing all
  platform-agnostic logic (parsing, validation, source resolution, playback
  control, session state machine, line protocol).
- A new Cargo workspace member `shepherd-media` containing the Linux binary
  (CLI entry, libmpv linkage via `libmpv2`, egui-based browse UI).
- A schema document and a reference test corpus.
- Integration points for `shepherdd` to invoke `shepherd-media`.

### Out of scope (this document)

- Android implementation. The spec only requires that `shepherd-media-core` be
  buildable for `aarch64-linux-android` without changes; it does not specify the
  Android shell, manifest, or launcher-replacement logic.
- iOS implementation. Permanently out of scope per project decision.
- DRM / subscription-service handling. These belong to issues #2 and #10.
- Library file editing. `shepherd-media` only reads library files.

### Non-negotiable constraints (from the parent project)

These come from `shepherd-launcher`'s stated non-goals and the implementing
agent must not violate them:

1. No telemetry. No analytics. No phone-home. No PII collection of any kind.
2. No DRM circumvention. URIs that resolve to DRM-protected streams must be
   rejected at validation time, not at playback time.
3. The library file is the sole source of truth for content. No directory
   scanning, no auto-discovery, no recommendations.
4. One foreground item at a time. No queueing, no autoplay, no "up next."

---

## 1. Crate layout

The implementing agent should add two crates to the existing workspace:

```
crates/
  shepherd-media-core/      # NEW: platform-agnostic library
    Cargo.toml
    src/
      lib.rs
      library.rs            # Library, Item, Source types; parsing
      schema.rs             # validation, error types
      resolver.rs           # platform/source selection
      player.rs             # PlayerHandle trait, libmpv-rs implementation
      session.rs            # browse/play state machine
      protocol.rs           # stdout line protocol for shepherdd
      uri.rs                # URI classification (local, http, youtube, drm-rejected)
    tests/
      fixtures/             # reference corpus (see §7)

  shepherd-media/           # NEW: Linux binary
    Cargo.toml
    src/
      main.rs               # CLI dispatch
      cli.rs                # clap definitions
      ui/
        mod.rs
        grid.rs             # egui poster grid
        theme.rs
      platform.rs           # PlatformInfo::current() returns Linux
```

### Cargo.toml constraints

`shepherd-media-core/Cargo.toml`:

- **MUST NOT** depend on any crate that pulls in X11, Wayland, GTK, or other
  Linux-specific UI/system libraries. `[android-portability]`
- **MUST NOT** depend on `tokio` with the `process` feature, or any process-spawning
  crate. The core controls libmpv via FFI, not via spawning. `[android-portability]`
- **SHOULD** use `libmpv2` (or `libmpv-rs`, whichever has a maintained Android-NDK
  build path; `libmpv2` is currently preferred) as the only mpv binding.
  `[android-portability]`
- File I/O is permitted (reading library files, posters) via `std::fs`.
- Network I/O is **not** permitted in the core. Poster fetching for HTTP URLs
  happens in the platform binary, which passes already-loaded bytes to the core.
  `[android-portability]`

`shepherd-media/Cargo.toml`:

- May depend on `egui`, `eframe`, `clap`, `reqwest` (for poster prefetch), and
  Linux-specific crates as needed.
- Links against system libmpv via `libmpv2`'s default features.

### Workspace integration

The existing `Cargo.toml` at the repo root has a `[workspace] members` array.
The agent should add `crates/shepherd-media-core` and `crates/shepherd-media`
to it. Existing crates must continue to build unchanged.

---

## 2. Library file format

### 2.1 Format

TOML, UTF-8, file extension `.toml`. JSON parsing is **not** required for the
Linux implementation. (The Android implementation may add JSON later; the schema
is identical.)

### 2.2 Schema

```toml
schema_version = 1                  # MUST equal 1; reject anything else
library_id = "kids-movies"          # ASCII, [a-z0-9-]+, 1..64 chars
title = "Movies"                    # display name, 1..128 chars

[[items]]
id = "big-buck-bunny"               # unique within library, [a-z0-9-]+, 1..64 chars
title = "Big Buck Bunny"            # 1..256 chars
kind = "video"                      # "video" | "audio"; reject others
category = "entertainment"          # OPTIONAL, free-form tag, 1..64 chars
poster = "posters/bbb.jpg"          # OPTIONAL; relative path, http://, or https://
duration_seconds = 596              # OPTIONAL; informational only, not enforced

[[items.sources]]
platforms = ["linux"]               # array of "linux" | "android" | "*"
uri = "file:///srv/media/bbb.mp4"   # see §2.3 for accepted schemes

[[items.sources]]
platforms = ["*"]
uri = "https://www.youtube.com/watch?v=YE7VzlLtp-4"
player_hint = "mpv"                 # OPTIONAL; advisory only, currently always "mpv"
```

### 2.3 URI classification

The `uri.rs` module classifies every URI into exactly one of:

- `Local(PathBuf)` — `file://` scheme. Path must be absolute. The core does
  **not** check existence (that's the player's job at playback time); it only
  validates the URI form.
- `DirectHttp(Url)` — `http://` or `https://` scheme, where the path ends in a
  recognized media extension (`.mp4`, `.mkv`, `.webm`, `.mov`, `.m4v`, `.mp3`,
  `.flac`, `.opus`, `.ogg`, `.m4a`, `.wav`) **or** the URL matches a known
  HLS/DASH manifest pattern (`.m3u8`, `.mpd`).
- `YouTube(Url)` — host matches `youtube.com`, `www.youtube.com`, `m.youtube.com`,
  `youtu.be`, or `youtube-nocookie.com`. mpv handles these via yt-dlp.
- `RejectedDrm(Url)` — host or scheme matches a known DRM/subscription service
  list (see §2.4). Validation **fails** if any source resolves to this class.
- `Unknown(Url)` — anything else (other HTTP(S) URLs not covered above).
  Validation **succeeds** but emits a warning. mpv will be tried at playback time.

### 2.4 DRM/subscription rejection list

Validation fails with a clear error if any source's URI host matches any of:

```
netflix.com, www.netflix.com
disneyplus.com, www.disneyplus.com
hulu.com, www.hulu.com
max.com, hbomax.com, play.hbomax.com
primevideo.com, www.amazon.com/gp/video, www.amazon.com/Prime-Video
appletv.apple.com, tv.apple.com
peacocktv.com, www.peacocktv.com
paramountplus.com, www.paramountplus.com
spotify.com, open.spotify.com
music.apple.com
```

Or any URI with scheme matching: `widevine:`, `playready:`, `fairplay:`.

The error message must include the rejected URI, the matched rule, and a pointer:
"Subscription/DRM services are out of scope for shepherd-media. See
shepherd-launcher issues #2 and #10."

This list is in `uri.rs` as a `const &[&str]`. Adding entries is a
straightforward PR; the agent should not try to be clever about it.

### 2.5 Schema versioning

`schema_version = 1` is the only currently valid value. Reject other values with
"unsupported schema_version: N (this build supports: 1)". This is forward
compatibility insurance; do not implement migration logic.

### 2.6 Reserved fields

Unknown top-level keys and unknown keys within `[[items]]` are **errors**, not
warnings. Use `serde(deny_unknown_fields)` on every struct in `library.rs`.
This prevents silent typo-induced misconfigurations.

---

## 3. The Rust core API

### 3.1 Public types (`shepherd-media-core`)

```rust
pub struct Library {
    pub schema_version: u32,
    pub library_id: String,
    pub title: String,
    pub items: Vec<Item>,
    pub source_path: PathBuf,  // for resolving relative poster paths
}

pub struct Item {
    pub id: String,
    pub title: String,
    pub kind: ItemKind,
    pub category: Option<String>,
    pub poster: Option<PosterRef>,
    pub duration_seconds: Option<u64>,
    pub sources: Vec<Source>,
}

pub enum ItemKind { Video, Audio }

pub enum PosterRef {
    Local(PathBuf),     // resolved absolute path
    Remote(Url),
}

pub struct Source {
    pub platforms: Vec<Platform>,
    pub uri: ClassifiedUri,
    pub player_hint: Option<PlayerHint>,
}

pub enum Platform { Linux, Android, Any }

pub enum ClassifiedUri {
    Local(PathBuf),
    DirectHttp(Url),
    YouTube(Url),
    Unknown(Url),
    // RejectedDrm never appears in a valid Library; it's a parse-time error
}

pub enum PlayerHint { Mpv }

pub fn load_library(path: &Path) -> Result<Library, LibraryError>;
```

`load_library` performs: TOML parse → `serde` deserialize → URI classification →
DRM rejection check → ID uniqueness check → relative-path resolution. All errors
are reported with the offending file path and, where possible, line/column.

### 3.2 Source resolution

```rust
pub struct PlatformInfo {
    pub platform: Platform,
}

pub fn resolve_source<'a>(item: &'a Item, info: &PlatformInfo) -> Option<&'a Source>;
```

First-match-wins: iterate `item.sources`, return the first whose `platforms`
list contains either the current platform or `Any`. Return `None` if no source
matches (which means this item is not playable on this platform; the UI should
gray it out).

### 3.3 Player abstraction

The core defines:

```rust
pub trait PlayerHandle: Send {
    fn play(&mut self, source: &Source) -> Result<(), PlayerError>;
    fn stop(&mut self) -> Result<(), PlayerError>;
    fn is_playing(&self) -> bool;
    fn poll_event(&mut self) -> Option<PlayerEvent>;
}

pub enum PlayerEvent {
    Started,
    EndOfFile,
    Error(String),
    Closed,         // user closed the player window
}
```

The Linux build provides one implementation, `LibmpvPlayer`, in
`shepherd-media-core/src/player.rs`. It uses `libmpv2` directly. Critical
configuration:

- `vo=gpu` (or platform default; let mpv decide)
- `fullscreen=yes`
- `osc=no` (no on-screen controller — the kid doesn't need a seek bar)
- `input-default-bindings=no` — disable mpv's default keybindings
- `input-vo-keyboard=no` — disable keyboard input to mpv
- `keep-open=no` — exit on EOF rather than pausing on last frame
- `ytdl=yes` — enable yt-dlp for YouTube URLs
- `ytdl-format=bestvideo[height<=?1080]+bestaudio/best[height<=?1080]/best`
  — cap at 1080p to avoid burning bandwidth on 4K streams a kid won't notice

`[android-portability]` On Android, `LibmpvPlayer` is replaced by a different
implementation of the same trait that uses libmpv via JNI. The trait must remain
the public surface; do not let libmpv-specific types leak into `session.rs`.

### 3.4 Session state machine

The session state machine is the core's most important piece. It coordinates
the browse-vs-play distinction that matters for shepherdd time accounting.

```rust
pub enum SessionState {
    Browsing,                    // user is looking at the library grid
    Playing { item_id: String }, // mpv is up, playback is active
    Stopping { item_id: String }, // stop requested, waiting for player to close
    Exiting,                     // session ending, will terminate process
}

pub struct Session {
    library: Library,
    state: SessionState,
    player: Box<dyn PlayerHandle>,
    protocol: ProtocolEmitter,
}

impl Session {
    pub fn new(library: Library, player: Box<dyn PlayerHandle>) -> Self;
    pub fn handle_input(&mut self, input: SessionInput);
    pub fn tick(&mut self);  // call regularly; polls player events
    pub fn state(&self) -> &SessionState;
}

pub enum SessionInput {
    SelectItem(String),    // user picked an item from the grid
    StopPlayback,           // user pressed back/escape during playback
    ExitSession,            // user requested to exit the whole shepherd-media session
}
```

Transitions:

| From | Input/Event | To | Protocol emitted |
|------|-------------|----|--------------------|
| `Browsing` | `SelectItem(id)` (resolves to a source) | `Playing { id }` | `STARTED_PLAYBACK item=<id>` |
| `Browsing` | `SelectItem(id)` (no source resolves) | `Browsing` | `WARNING item=<id> reason=no-source` |
| `Browsing` | `ExitSession` | `Exiting` | `EXIT reason=user` |
| `Playing` | `PlayerEvent::EndOfFile` | `Browsing` | `RETURNED_TO_MENU item=<id> reason=eof` |
| `Playing` | `PlayerEvent::Closed` | `Browsing` | `RETURNED_TO_MENU item=<id> reason=closed` |
| `Playing` | `PlayerEvent::Error(msg)` | `Browsing` | `ERROR item=<id> message=<msg>` then `RETURNED_TO_MENU item=<id> reason=error` |
| `Playing` | `StopPlayback` | `Stopping { id }` | (none yet) |
| `Stopping` | `PlayerEvent::Closed` | `Browsing` | `RETURNED_TO_MENU item=<id> reason=user` |
| `Browsing` or `Playing` | (SIGTERM) | `Exiting` | `EXIT reason=signal` |

This is the entire state machine. There is deliberately no "paused" state; pause
is mpv's internal concern, invisible to shepherdd.

### 3.5 Line protocol

`shepherd-media` writes one line per event to **stdout**. Each line is:

```
<EVENT> <key>=<value> [<key>=<value>...]
```

Values containing spaces or `=` must be percent-encoded. No JSON; this is meant
to be `read`-line-able in shell scripts and trivially parseable in Rust.

Events emitted by the running process:

- `READY library_id=<id> item_count=<n>` — at startup, after library loads
- `STARTED_PLAYBACK item=<id> kind=<video|audio> source=<uri-class>`
  where `<uri-class>` is one of `local`, `direct-http`, `youtube`, `unknown`
- `RETURNED_TO_MENU item=<id> reason=<eof|closed|user|error>`
- `WARNING item=<id> reason=<machine-readable-tag>`
- `ERROR item=<id> message=<percent-encoded>`
- `EXIT reason=<user|signal|crash>` — final line before process exits

Stderr is for human-readable logging (using `tracing` or `env_logger`) and is
not part of the protocol.

shepherdd can use this stream to:

- Start its time-accounting clock on `STARTED_PLAYBACK`, pause it on
  `RETURNED_TO_MENU`. This is what makes "browsing the menu doesn't burn quota"
  honest.
- Issue a clean stop via SIGTERM when a time limit hits. The process will
  receive it, drive the state machine to `Exiting`, close the player cleanly,
  emit `EXIT reason=signal`, and exit.

---

## 4. CLI

```
shepherd-media <SUBCOMMAND>

Subcommands:
  validate <library.toml>
      Parse and validate the library file. Exit 0 if valid; print errors to
      stderr and exit non-zero if not. Prints a one-line summary on success:
      "OK: library_id=<id> items=<n>".

  play --library <library.toml> --item <item-id>
      Direct-play mode. Resolve the item's source for this platform, launch
      mpv, exit when playback ends. shepherdd uses this for items it has been
      configured to launch directly (e.g. a single video that's its own
      "activity" in the launcher).

  browse --library <library.toml>
      Browse mode. Open the egui poster grid. User selects items; each playback
      is a state-machine transition, not a process exit. Process runs until
      the user explicitly exits or shepherdd sends SIGTERM.

Global flags:
  --log-level <error|warn|info|debug|trace>   default: info
  --no-protocol                                suppress stdout protocol
                                               (useful when running by hand)
```

Exit codes:

- 0: success
- 1: library validation error
- 2: invocation error (bad flags, missing files, etc.)
- 3: player error that wasn't recoverable
- 4: signal-driven shutdown (this is success-ish, but distinguishable)

---

## 5. Linux UI (browse mode)

The browse-mode UI is built with `egui` + `eframe`, fullscreen, with a
dark-themed poster grid. UI requirements:

- **Fullscreen by default**, no window decorations. The user is in a kiosk;
  they don't need a title bar.
- **Grid of posters**, 4 columns on a typical 1080p display; recompute columns
  for window width. Each cell shows the poster (or a generated text placeholder
  if no poster) and the item title.
- **Keyboard and gamepad navigation.** Arrow keys / D-pad to move, Enter / A
  to select, Escape / B to exit. shepherd-launcher already handles gamepad
  input elsewhere; reuse the same crate (`gilrs` or whatever the existing
  codebase uses — agent should check `Cargo.lock` and prefer the existing
  choice).
- **Items with no resolved source** for this platform are shown grayed out and
  cannot be selected.
- **No mouse support required** for v1. Kiosk inputs are keyboard and gamepad.
- **Posters** are loaded asynchronously by the binary (not the core). Local
  posters are read from disk on startup; remote posters are fetched on startup
  with a 5-second timeout per poster, and if the fetch fails the placeholder
  is used. Posters are not re-fetched on subsequent launches without an
  explicit cache-busting flag (out of scope for v1).

`[android-portability]` The UI is deliberately the *one part* of the Linux
binary that won't port directly to Android. That's fine — Android will use
Compose or a leanback-style native UI. The contract between UI and core is
just the `Session::handle_input` method, which is platform-neutral.

---

## 6. shepherdd integration

`shepherd-media` is invoked by shepherdd as a normal activity. Two integration
patterns must be supported.

### 6.1 Direct-play activity

A library item is registered as its own shepherd-launcher activity, e.g.:

```toml
[[entries]]
id = "big-buck-bunny"
label = "Big Buck Bunny"
icon = "media-video"

[entries.kind]
type = "command"
program = "shepherd-media"
args = ["play", "--library", "/etc/shepherd/movies.toml", "--item", "big-buck-bunny"]
```

shepherdd treats this exactly like any other process activity — spawn,
time-track, send SIGTERM on limit, reap. The protocol stream is informational;
shepherdd does not need to read it for direct-play mode.

### 6.2 Browse-mode activity

A whole library is registered as one activity:

```toml
[[entries]]
id = "movies-library"
label = "Movies"
icon = "folder-videos"

[entries.kind]
type = "command"
program = "shepherd-media"
args = ["browse", "--library", "/etc/shepherd/movies.toml"]
read_protocol = true                # NEW shepherdd capability

[entries.kind.time_accounting]
mode = "playback_only"              # only count time when STARTED_PLAYBACK is active
```

This requires shepherdd to gain the ability to read the child process's stdout
and use `STARTED_PLAYBACK` / `RETURNED_TO_MENU` events to drive its time
accounting. **That shepherdd-side change is out of scope for this spec** — it
should be tracked as a separate issue. For an agent implementing only the
shepherd-media side, treat shepherdd as a black box that sends SIGTERM when it
wants the session to end, and emit the protocol unconditionally.

---

## 7. Test corpus

Under `crates/shepherd-media-core/tests/fixtures/` create these library files
plus a `validity.toml` manifest declaring which should pass and which should
fail validation:

- `valid-local-only.toml` — three items, all `file://` URIs, all with posters
- `valid-youtube-only.toml` — three items, all YouTube URIs
- `valid-mixed.toml` — five items, mix of local, YouTube, direct HTTP
- `valid-platform-fallback.toml` — items with separate Linux and Android sources
- `valid-no-poster.toml` — items missing the optional poster field
- `invalid-bad-schema-version.toml` — `schema_version = 99`
- `invalid-duplicate-id.toml` — two items with the same `id`
- `invalid-drm-rejected.toml` — a Netflix URL
- `invalid-unknown-key.toml` — typo'd field name (`titel` instead of `title`)
- `invalid-empty-sources.toml` — an item with `sources = []`
- `invalid-no-sources-for-any-platform.toml` — item with sources, but none match `linux` or `*`
  (this is a *warning* in load_library but a *resolution failure* at play time;
  document the distinction)

Integration tests in `crates/shepherd-media-core/tests/`:

- `parse.rs` — load each fixture, assert pass/fail per the manifest
- `resolve.rs` — for each valid fixture, assert source resolution behaves as expected
- `protocol.rs` — drive the state machine through scripted inputs/events,
  assert the emitted protocol stream matches a golden file

---

## 8. Build, lint, CI

- Add `crates/shepherd-media-core` and `crates/shepherd-media` to the workspace.
- Both crates must pass `cargo clippy --all-targets -- -D warnings` and
  `cargo fmt --check` under the existing repo settings (`clippy.toml` is
  already at the root).
- Add a CI job that runs `cargo build --target aarch64-linux-android -p shepherd-media-core`
  using `cross` or `cargo-ndk`. **This job's job is to fail loudly if someone
  accidentally adds a Linux-specific dependency to the core.** It does not need
  to produce a usable Android artifact. `[android-portability]`
- Add a CI job that runs the integration tests in §7.

---

## 9. Suggested implementation order for the agent

Do these in order. Each step should be a self-contained commit (or PR) that
leaves the workspace building and tests passing.

1. **Crate skeletons.** Create both crates with empty `lib.rs`/`main.rs`,
   wire up the workspace, get `cargo build` green. Add the
   `aarch64-linux-android` CI job; it should pass at this point because the
   core has no code.

2. **Library types and parsing.** Implement `library.rs`, `uri.rs`, and the
   DRM rejection list. Add the test corpus and the `parse.rs` integration test.
   At this point `shepherd-media validate` works; implement that subcommand
   end-to-end and write a CLI test.

3. **Source resolution.** Implement `resolver.rs` and the `resolve.rs` test.
   Trivial in code, but lock in the platform/source matching semantics with
   tests before the player work piles on top.

4. **Player abstraction and libmpv backend.** Define the `PlayerHandle` trait
   in the core. Implement `LibmpvPlayer` using `libmpv2`. Wire up
   `shepherd-media play --library ... --item ...` end-to-end. This is the
   first commit where the binary actually plays media.

5. **Session state machine and protocol.** Implement `session.rs` and
   `protocol.rs`. Add the `protocol.rs` integration test with golden files.
   Wire `play` mode through the state machine so its protocol output matches
   `browse` mode for a single item.

6. **Browse UI.** Implement the egui grid in `shepherd-media/src/ui/`. This
   is the largest single piece of work; budget accordingly. End state:
   `shepherd-media browse --library ...` works as an activity.

7. **Documentation.** Add `docs/shepherd-media.md` describing how parents
   author library files, with examples mirroring the test corpus.
   Update the main README to mention `shepherd-media` and link to the
   document. Add an example `[[entries]]` block to `config.example.toml`
   for both direct-play and browse activities.

8. **shepherdd protocol-reader hook (optional / separate issue).** If the
   agent has scope, add the `read_protocol = true` and
   `time_accounting.mode = "playback_only"` support to shepherdd. Otherwise
   file a follow-up issue and stop.

---

## 10. Things the agent should *not* do

- Do not add a configuration file for `shepherd-media` itself. The library
  file is the only configuration. Anything that would go in a separate config
  file should either be a CLI flag or it doesn't belong.
- Do not implement playlists, queues, autoplay, "watch next," recommendations,
  history tracking, or resume-from-where-you-left-off. These are explicit
  non-features.
- Do not add network calls from the core. Poster fetching belongs in the
  binary, not the core. `[android-portability]`
- Do not shell out to `mpv` as a subprocess. Use `libmpv2`. The temptation
  to "just call `Command::new("mpv")`" will be strong; resist it. The Android
  port can't do that, and the protocol-stream design depends on the player
  living in-process. `[android-portability]`
- Do not add yt-dlp as a direct dependency. It is invoked transitively by
  libmpv when `ytdl=yes`. The user is responsible for having `yt-dlp`
  installed; document this in `docs/shepherd-media.md` and detect its
  absence at startup with a clear warning.
- Do not implement library-file hot-reload. If a parent edits the library
  file, they restart the activity. This is consistent with the rest of
  shepherd-launcher.
- Do not add any "kid mode" UI affordances beyond what's specified. No
  cute animations, no sound effects, no celebratory transitions. The UI is
  a poster grid; the content is the experience.

