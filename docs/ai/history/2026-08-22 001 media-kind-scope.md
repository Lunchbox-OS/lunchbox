# A first-class `media` activity kind — scope (issue #127)

> Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/127>
> Related: #129 (`retroarch` kind), #2/#75 (`android` kind) — same "promote a
> shepherd-owned sidecar from `type = "process"` to a real kind" shape.

## Prompt

> scope out #127

## Issue text

> **Media kind**
>
> Rather than simply calling `shepherd-media` via the "process" type, we should
> expose a proper "media" kind. This way, shepherdd can be directly aware of the
> `shepherd-media` args and potentially perform video caching in the background,
> not just when `shepherd-media` itself is running.

## What exists today

There are two half-built things that this issue joins up.

**1. A `media` kind that is scaffolding only.** `EntryKind::Media { library_id,
args }` already exists end to end — `crates/shepherd-config/src/schema.rs:460`,
`crates/shepherd-api/src/types.rs:151`, the `EntryKindTag::Media` capability set
(`crates/shepherd-host-api/src/capabilities.rs:65`), the launcher tile icon
(`crates/shepherd-launcher-ui/src/tile.rs:85`), the generated Kotlin
(`WireTypes.generated.kt:304`) and the TS tag union
(`shepherd-webui/src/api/types.ts:38`). Validation only checks that
`library_id` is non-empty (`crates/shepherd-config/src/validation.rs:263`).

But the launch path is a stub. `crates/shepherd-host-linux/src/adapter.rs:550`:

```rust
EntryKind::Media { library_id, args: _ } => {
    // For media, we'd typically launch a media player
    // This is a placeholder - real implementation would integrate with a player
    let argv = vec!["xdg-open".to_string(), expand_tilde(library_id)];
    ...
}
```

`args` is dropped on the floor, and `library_id` is treated as a filesystem
path. Nothing in the tree configures this kind, and nothing could usefully.

**2. Media activities that actually work, wired as `type = "process"`.** All
four media entries in `config.example.toml:786–880` spell out a `shepherd-media`
invocation by hand:

```toml
[entries.kind]
type = "process"
command = "shepherd-media"
args = ["browse", "--library", "~/.config/shepherd/movies.toml"]
```

shepherdd knows nothing about these beyond "a process called `shepherd-media`".

**3. Video caching that only runs inside `shepherd-media`.**
`crates/shepherd-media/src/video_cache.rs` (726 lines) holds a `VideoCache` with
a background download thread (yt-dlp / direct HTTP), a `.done` sentinel that
records the yt-dlp format selector, mtime-based LRU eviction against a 10 GiB
cap, and a `CachingPlayer` wrapper that substitutes cached files at play time.
It is reachable only while the binary runs: `queue_all` at browse launch
(`crates/shepherd-media/src/main.rs:225`) and `queue_after_play` on EOF. Close
the activity and downloading stops.

## What "a proper media kind" buys

| | today (`process`) | with a `media` kind |
|---|---|---|
| Config | free-text argv the admin must keep in sync with the CLI | typed fields, validated at load |
| Bad `--item` | fails at launch, child sees a flash | caught by `shepherd-admin config validate` |
| `--connectivity-check` | restated per entry, duplicating `internet.check` | inherited from policy |
| Caching | only while the activity is open | daemon-side, ahead of time |
| Future protocol reader | shepherdd can't tell `shepherd-media` from any other process | it knows |

## Proposed config shape

```toml
[[entries]]
id = "movies-library"
label = "Movies"
icon = "folder-videos"

[entries.kind]
type    = "media"
library = "~/.config/shepherd/movies.toml"  # path, .m3u/.m3u8, or YouTube playlist URL
mode    = "browse"                          # "browse" (default) | "play"
quality = "1080p"                           # default 1080p
sort_by = "library"                         # library|title|id|kind|category|duration
reverse = false
resume  = true
```

Direct-play is the same kind with `mode = "play"` and an `item`:

```toml
[entries.kind]
type    = "media"
library = "~/.config/shepherd/movies.toml"
mode    = "play"
item    = "big-buck-bunny"
```

Notes on the field set:

- **`library` replaces `library_id`.** The existing name is wrong for what the
  value is: `shepherd-media` takes a library *source* (a file path or a playlist
  URL), while `library_id` is the identifier *inside* that file — the one that
  names the resume-state file. Renaming is free: the kind has never launched
  anything real, so there is no configuration in the wild to migrate.
- **`connectivity_check` is inherited, not restated.** shepherdd already knows
  `entries.internet.check` and `service.internet.check`
  (`crates/shepherd-config/src/internet.rs:149`), and the example config today
  makes the admin paste the same URL into the media entry's args. The adapter
  should forward the entry's check, falling back to the service's, for
  `mode = "browse"`. See the open questions for the opt-out spelling.
- **No `env` / `cwd` / raw `args` escape hatch.** Anyone who needs one still has
  `type = "process"`, which keeps working unchanged. If an escape hatch turns out
  to be needed, `extra_args = [...]` can be added later without a breaking change.
- **`log_level` and `--no-protocol` are not exposed.** They are debugging flags;
  shepherdd should pick them (and will want the protocol on once it reads it).

Service-level knobs for the daemon-side cache:

```toml
[service.media]
prefetch = true               # default: on iff any media entry exists
cache_max_bytes = 10737418240 # 10 GiB; supersedes SHEPHERD_MEDIA_VIDEO_CACHE_MAX_BYTES
prefetch_while_session_active = false
```

## Wire changes

`EntryKind` is in the wire schema (`crates/shepherd-wire-codegen/src/wire_schema.rs:29`),
so redefining the `Media` variant regenerates `WireTypes.generated.kt` and trips
the drift test until it is checked in. `EntryView` only carries `kind_tag`
(`crates/shepherd-api/src/types.rs:350`), so the launcher, HUD, and web UI are
unaffected — the TS side has only the tag union, which does not change.

`Quality` and `SortBy` live in `shepherd-media-app` / `shepherd-media` and derive
`clap::ValueEnum`, not `schemars::JsonSchema`. Recommendation: **mirror them as
plain enums in `shepherd-api`** and convert at the adapter boundary, rather than
making `shepherd-api` depend on the media crates. Add a test that asserts the
variant sets match so they cannot silently diverge.

## Launch path

Replace the `xdg-open` stub with real argv construction, and **extract it into a
free function** (`fn media_argv(kind, policy_check) -> Vec<String>`) so it can be
unit-tested — the current `spawn` builds argv inline in a 200-line match, and
`adapter.rs` has no argv tests at all.

```
shepherd-media browse --library <expanded> --quality 1080p --sort-by library \
  [--reverse] [--resume] [--connectivity-check <url>]
```

Also needed: tilde expansion on `library` (but *not* on a URL), and the
`command_name` fallback used for supervision — `argv[0]` is already
`shepherd-media`, so that falls out correctly.

## Background caching — the substantial half

This is where the issue's real payload is, and where the work is not mechanical.

### What has to move

`video_cache.rs` is inside the `shepherd-media` binary crate, which pulls
`eframe`, `egui`, `gilrs`, and libmpv. shepherdd cannot depend on it. The
download worker, the `.done`/selector protocol, and the LRU eviction need to be
hoisted into a crate both binaries can use — either a new
`shepherd-media-cache`, or a Linux-gated module of `shepherd-media-app` (which
today is dependency-light: serde, toml, thiserror, log). The `CachingPlayer`
wrapper stays behind in `shepherd-media`, since it is a `PlayerHandle`.

shepherdd also needs to *read libraries* to know what to prefetch:
`shepherd-media-core` with `default-features = false` gives library parsing and
source resolution without libmpv (the `libmpv` feature is already gated for
exactly this reason). The yt-dlp playlist fetch
(`crates/shepherd-media/src/youtube.rs`) and `paths.rs` hoist alongside.

### Two defects that only bite once two processes cache

**Cache keys collide across libraries.** Files are named `<item_id>.<ext>` in a
single flat `$XDG_CACHE_HOME/shepherd/media/videos/` directory
(`find_cached_file`, `video_cache.rs:336`). `item_id` is unique only *within* a
library. Two libraries that both define `intro` share one cache file today —
latent, because one process handles one library at a time. A daemon prefetching
every configured library at once makes it routine. Fix: key by
`<library_id>/<item_id>`, or by a hash of the resolved source URL.

**Nothing arbitrates two writers.** shepherdd's worker and a running
`shepherd-media` can both decide `intro` is `Absent` and start downloading into
the same `.part` file. The `.done` sentinel makes a *reader* safe but does
nothing for concurrent writers. Fix: an `flock`-based claim (or `O_EXCL` lock
file) held for the duration of a download, with the loser skipping.

Eviction is less alarming than it looks: shepherdd unlinking a file that
`shepherd-media` has open for playback is safe on Linux — the reader keeps its
inode. The cost is a silent re-download next time. Worth noting that mtime is
bumped only at play *start* (`CachingPlayer::play`), so a two-hour film is an
eviction candidate for most of its own runtime.

### When the daemon should prefetch

Prefetch is bandwidth and CPU that a child's session is also using, so the
scheduler matters more than the mechanism:

- **On startup and on config reload** — enumerate media entries, load each
  library (playlist fetches are already cached 6h), resolve remote sources,
  queue what is `Absent`.
- **Only while the internet is up.** Reuse `InternetMonitor`'s existing state
  rather than probing again (`crates/shepherdd/src/internet.rs`).
- **Not while a session is active**, by default — pause the worker on
  `SessionStarted`, resume on `SessionEnded`. A yt-dlp download competing with
  mpv on the same box is exactly the "shepherd-media performance" problem from
  `docs/ai/history/2026-07-28 001 shepherd-media performance investigation.md`.
- **Prefetch never evicts.** The existing `evict_after: false` path already has
  this discipline: a background prefetch skips when the cache is at capacity
  rather than displacing something the child has watched. Keep it.

The task itself is a peer of `internet.rs` / `input_devices.rs` — a
`crates/shepherdd/src/media.rs` spawned from `Service::run`
(`crates/shepherdd/src/main.rs:440–475`), which is also where the Steam preload
precedent sits (`main.rs:211–227`).

### Readiness gating (optional, cheap)

Steam already hides its tiles until preload finishes via
`set_kind_readiness(EntryKindTag::Steam, false)` and
`HostEvent::KindReadinessChanged`. The same mechanism could gate media tiles on
`shepherd-media` being on `PATH` (and `yt-dlp` when any library source is a
YouTube URL). Today a missing `yt-dlp` surfaces as a launch that dies on the
child's screen. Small, self-contained, and worth folding into the same branch —
but it is not what the issue asks for, so it is the first thing to cut.

## Out of scope

- **Reading the stdout protocol for playback-only time accounting.**
  `docs/shepherd-media.md:295` already calls this "tracked separately". It is a
  larger change than it looks: `ManagedProcess::spawn` never pipes stdout (only
  an unused `log_path` redirect, `process.rs:719`), and the firewall path wraps
  the activity in `pkexec systemd-run --scope`, which puts a privilege boundary
  between shepherdd and the stream. Worth its own issue. A `media` kind is a
  prerequisite for it, which is an argument for doing this issue first.
- **The Android app (`shepherd-media-android`).** It has its own settings UI and
  no shepherdd.
- **Poster prefetch.** `posters.rs` has a separate TTL cache; same daemon task
  could warm it later, but posters are small and load fast.

## Suggested phasing

| Phase | Content | Rough size |
|---|---|---|
| 1 | The kind: schema, validation, `shepherd-api`, `media_argv` + tests, Kotlin regen, `config.example.toml` conversion of all four media entries, `docs/shepherd-media.md` integration section | ~1 day; no behavior change beyond a better config surface |
| 2 | Hoist `video_cache.rs` into a shared crate; fix the library-scoped cache key; add the download lock | refactor, user-invisible, but the migration of existing cache files needs a decision (simplest: treat the old flat layout as cold and let it age out) |
| 3 | `crates/shepherdd/src/media.rs` prefetcher + `[service.media]` + internet/session gating | the actual issue payload |
| 4 | Readiness gating; protocol reader (separate issue) | optional |

Phase 1 is independently shippable and is what closes the config half of the
issue; phase 3 is what closes the caching half.

## Test plan

- **Unit** — `media_argv` for both modes, tilde expansion vs. URL passthrough,
  connectivity-check inheritance; config validation (`item` required iff
  `mode = "play"`, unknown `quality`/`sort_by` rejected at load); cache-key
  derivation and the download lock.
- **Config** — `config.example.toml` must still pass
  `shepherd-admin config validate` (CLAUDE.md requires it).
- **Wire** — regenerate and check in `WireTypes.generated.kt`; the drift test
  fails until then.
- **End-to-end** — the `headless-dev` skill: launch the converted `movies-library`
  entry and screenshot the poster grid, confirming the argv change is faithful to
  what `type = "process"` produced.
- **Prefetch** — point a test library at a small remote file, run shepherdd with
  no session, assert the file lands in the cache and that a subsequent launch
  plays locally.

## Open questions

1. ~~**Connectivity-check opt-out spelling.**~~ **Answered:** a flag on the
   existing per-activity block — `[entries.internet] forward_check = false`.
   Built in phase 1.
2. ~~**`cache_max_bytes` ownership.**~~ **Answered:** leave it where it is.
   `SHEPHERD_MEDIA_VIDEO_CACHE_MAX_BYTES` stays the only knob; no
   `[service.media]` field, and nothing for phase 1 to do.
3. ~~**Does phase 3 prefetch entries that are currently unavailable?**~~
   **Answered:** the suggested rule — every enabled entry regardless of
   schedule, skipping disabled ones. Built in phase 3.
4. ~~**Old flat cache files.**~~ **Answered:** leave them; they are ordinary
   eviction candidates and age out. Built in phase 2.

## As built — phase 1

> Follow-up prompt:
>
> > Do phase 1. For the connectivity check disabling, add a flag to the existing
> > per-activity connectivity check block. For the cache size, just use
> > shepherd-media's existing environment variable

Phase 1 shipped as scoped. What the diff actually contains, and where it
diverged:

**The kind.** `RawEntryKind::Media` and `EntryKind::Media` both carry
`library` / `mode` / `item` / `quality` / `sort_by` / `reverse` / `resume`, with
`RawMediaMode` / `RawMediaQuality` / `RawMediaSortBy` mirrored as `MediaMode` /
`MediaQuality` / `MediaSortBy` in `shepherd-api` — the Raw-mirror pattern the
crate already uses for `RawInputCompat` → `InputCompatMode`, and what keeps
`shepherd-api` off the media crates.

**Where the mirror is tested.** The scope said `shepherd-host-linux` would
assert the two enum sets agree; it can't — it doesn't depend on the media
crates either. The guard landed in `crates/shepherd-media/src/cli.rs` instead
(`shepherd-api` as a dev-dependency), and it runs both ways: every flag string
`MediaQuality::as_flag` / `MediaSortBy::as_flag` emits must parse as a clap
value, and the CLI must not grow a preset the API can't name.

**Connectivity check.** `[entries.internet] forward_check` (default `true`).
The resolved target — the entry's, else `service.internet.check` — reaches the
adapter as `SpawnOptions::connectivity_check`, populated in
`shepherd-management`'s launch path, which is the only place `SpawnOptions` is
built. `forward_check = false` sends `None`.

This turned out to be a real simplification in the example config: the
`youtube-channel` entry had been repeating
`https://connectivitycheck.gstatic.com/generate_204` in its args, which is
exactly what `[service.internet] check` above it already says. It now inherits,
and the flag is what an admin reaches for to opt out.

**A codegen bug this surfaced.** `MediaQuality`'s wire values start with digits
(`1080p`), and `constant_name` in `shepherd-wire-codegen` upcased them straight
into `enum class MediaQuality { 1080P }` — not a legal Kotlin identifier, so the
companion app would not have compiled. `constant_name` now prefixes `Q_` for a
leading digit and maps non-alphanumerics to `_`; the wire value itself is
untouched in `@SerialName`. No existing constant changed name.

**Icons.** `autodetect_icon` no longer returns `None` for media: `browse` gets
`folder-videos`, `play` gets `video-x-generic`. Small, but the kind now has a
sensible tile without an explicit `icon`.

**Validation.** Empty `library`, `mode = "play"` without an `item`, and an
`item` set under `mode = "browse"` are all config errors. An unknown `quality`
or `sort_by` is a *parse* error from serde, so it can never reach the launch
path as a silent default — there is a test pinning that, since it is the kind of
guarantee that quietly disappears if someone adds `#[serde(other)]`.

**Verified.** `media_argv` unit tests cover both modes, tilde expansion vs. URL
passthrough, and check forwarding. End to end through the `headless-dev`
harness at a mocked `2026-08-22 14:00`, the converted entries produce:

```
shepherd-media browse --library /home/…/.config/shepherd/movies.toml \
  --quality 1080p --sort-by library \
  --connectivity-check https://connectivitycheck.gstatic.com/generate_204

shepherd-media play --library /home/…/.config/shepherd/movies.toml \
  --item big-buck-bunny --quality 1080p --sort-by library \
  --connectivity-check https://connectivitycheck.gstatic.com/generate_204
```

Browse paints the four-item poster grid under the HUD; play opens straight into
the player surface with no grid. Workspace tests, clippy, and `cargo fmt` are
clean, and `config.example.toml` passes validation.

**Not done, by design.** Nothing about caching moved — phases 2 and 3 are
untouched, and `SHEPHERD_MEDIA_VIDEO_CACHE_MAX_BYTES` remains the only cap.
`type = "process"` with `command = "shepherd-media"` still works and is still
the escape hatch for flags the kind does not expose (`--log-level`,
`--no-protocol`).

## As built — phase 2

> Follow-up prompt:
>
> > Do phase 2. For #1, use the hash. For #2, make the new crate. For #3, just
> > stream (which is what shepherd-media does today). #4. leave them

**The crate.** `crates/shepherd-media-cache` — keying, the download worker,
the on-disk layout, LRU eviction, and the lock. It takes `shepherd-media-core`
with `default-features = false`, so the mpv-backed `PlayerHandle` is absent
entirely and a daemon linking this pulls in no player. What stayed behind in
`shepherd-media` is `caching_player.rs`: the `PlayerHandle` wrapper that
substitutes a cached file at play time and queues after EOF, which is exactly
the part that cannot be shared because a daemon has no player.

`media_cache_dir` moved with the cache (`posters.rs` and `youtube.rs` now call
it there); `media_state_dir` stayed in `shepherd-media`, since resume positions
are state, not cache. `filetime` dropped out of `shepherd-media` entirely.

`shepherd-media-android` keeps its own `video_cache` module and is untouched —
it downloads through Android APIs, not yt-dlp.

**Keying.** The first 128 bits of the SHA-256 of the source URL, hex. `sha2` was
already in the lockfile, so this costs no new compile. Deliberately *not*
`DefaultHasher`, whose output is documented as unspecified and would orphan the
whole cache on a toolchain upgrade — the sort of thing that would look like a
mystery mass re-download months later.

This changed the public API in a way worth noting: `cached_path` and the queue
methods now take a `&Source` rather than an item id, because the key is derived
from the URL. That removed the item-id plumbing from the UI's offline filter,
which had been passing `&item.id` alongside a `source` it already had in hand.

**Locking.** `flock` on `<key>.lock`, non-blocking, held for the download; the
loser skips, and the winner re-checks cache state under the lock in case the
holder it lost to just committed that exact file. `flock` over a
presence-based lock for a specific reason: the kernel releases it when the
holder dies, and activities are SIGTERM'd mid-download every time a session
ends — a presence check would strand that item as permanently un-downloadable.

Lock files are never deleted, including by `remove_cached_item`, which now
skips them explicitly. Removing one while another process held it open would
leave the next claimant locking a fresh inode.

**Old files left in place**, as decided. They carry valid sentinels, so they are
ordinary LRU candidates: they count toward the cap and age out. Nothing will
ever ask for them by name again. There is a test pinning that, so a future
reader doesn't "fix" the apparent leak.

**Testing.** 23 unit tests (keying, lock contention, store classification,
eviction) plus 5 integration tests that drive the real worker against a loopback
HTTP server — queue, claim, fetch, commit, look up — covering the hash-named
commit, cache hits not re-downloading, two libraries with the same item id
getting two files, a prefetch at capacity being skipped rather than evicting,
and an after-play download evicting to make room. Loopback only, so nothing in
CI depends on the network.

Verified in the headless harness that the browse grid is pixel-identical to
before the refactor, and that a launch does now create `<hash>.lock` in the live
cache directory.

**Still open for phase 3**, unchanged: whether the daemon evicts against a lower
watermark (a cache that fills once stops accepting prefetches), a free-disk
floor, per-entry `quality` disagreeing across entries that share a library,
which entries get prefetched, pause policy, playlist refresh, and observability.

## As built — phase 3

> Follow-up prompt (answering the phase-3 questions in order):
>
> > do phase 3. for #5, put the selector in the key. for #6, invert only
> > download completion time so that truly unwatched videos are interpreted as
> > lack of interest and candidates for replacement. for #7, make this a
> > warning. for #8, yes, that. for #9, add both per-activity and global
> > opt-outs. for #10, make lack of yt-dlp combined with any detected YouTube
> > based media or playlists a warning. for #11, logs are fine.

### #5 — the selector is in the content key

`content_key = SHA-256(url ‖ NUL ‖ selector)`. Two entries over one library at
different `quality` settings now cache side by side instead of each launch
seeing the other's file as the wrong codec, deleting it, and re-downloading.

This deleted a whole state: `CacheState::StaleSelector` is gone, and with it the
compare-the-sentinel logic, because a file fetched under a different selector
now simply lives under a different name. `CacheState` is `Present`/`Absent`.

It also forced a **second** key. Eviction needs to know whether a video was
watched, and interest belongs to the video, not the rendition — watching at
1080p should protect the 480p copy. So `interest_key = SHA-256(url)` names one
file, `<ikey>.played`. Walking the directory yields content keys and a hash does
not run backwards, so the sentinel had to start recording the interest key;
its format is now `interest=…\nselector=…`, with the selector kept for
debugging only.

### #6 — unwatched content is what gets replaced

`Recency` is a two-class ordering: `Unwatched(Reverse<SystemTime>)` sorts below
`Watched(SystemTime)`, always. Every guess is spent before any watched file is
touched. Plain mtime had this backwards — a prefetch that landed a minute ago
looked "recently used" and outranked a film watched last week.

The inversion within the unwatched class means the *newest* speculative download
goes first. That is deliberate and it is what makes repeated sweeps converge:
prefetch walks a library in display order, so the newest file is the furthest
down the list. Evicting the head instead would have the next pass re-download
it immediately, forever.

Two bugs fell out of building this, both caught by tests rather than by reading:

- **Eviction to exactly the cap is a no-op**, so a full cache stayed full: the
  skip test is `>= max_bytes` and `evict_to_cap` returns early at `<=`. Prefetch
  now makes room to `max_bytes - 1` — strictly below, so there is room for
  something.
- **An after-play download evicted itself.** It arrives unwatched, so under the
  new ordering it was the first thing the trim that followed it discarded. The
  worker now writes the `.played` marker on an after-play commit, which is
  honest: that download exists *because* the child watched the video.

`queue_prefetch` may now recycle unwatched space rather than skipping outright,
which is what stops a filled cache going inert. It still cannot touch a watched
file: if everything cached has been watched, the guess is dropped.

### #7–#11 — the daemon task

`crates/shepherdd/src/media.rs`, a peer of `internet.rs`, driven entirely off
the event broadcast so it never reaches into the engine.

- **#8 — what gets prefetched:** every enabled `media` entry, schedule ignored
  (an activity outside its window today is exactly the one worth having ready
  tomorrow); `disabled` entries skipped. A `mode = "play"` entry prefetches only
  the item it launches, not its whole library.
- **#9 — opt-outs at both levels:** `service.media.prefetch` globally,
  `prefetch = false` under `[entries.kind]` per activity. Plus
  `service.media.prefetch_while_session_active` (default false): a download
  competing with a game, or with the video being watched right now, spends the
  child's CPU and bandwidth on content nobody asked for. Resumption waits 30s
  after a session ends so closing one activity and opening another doesn't
  spend the gap downloading.
- **#7 — free disk is a warning:** below `service.media.free_space_floor_bytes`
  (2 GiB default, 0 disables) the sweep logs a warning and stops. It warns
  *and* stops rather than warning alone, because prefetch is by definition
  optional and filling a kiosk's disk is a support call. The cache cap bounds
  the cache, not the volume it sits on.
- **#10 — missing yt-dlp is a warning:** at startup, if any media entry's
  library is a YouTube playlist URL or contains a YouTube source and `yt-dlp`
  is not runnable, shepherdd logs a warning naming the entries. Otherwise the
  failure is invisible until a child taps a tile and the activity dies.
- **#11 — logs only.** Nothing new on the wire.

`shepherdd` gained `shepherd-media-cache`, `shepherd-media-core`
(`default-features = false`, so no libmpv), and `shepherd-media-app` for the
`Quality` → selector mapping. The YouTube playlist fetch moved from
`shepherd-media/src/youtube.rs` into `shepherd-media-cache` as `playlist.rs` —
the daemon has to know what is in a playlist before it can prefetch it, and that
module already owned the playlist metadata cache.

### The bug the live run caught

The first headless boot swept **every library several times a second**. The
event loop treated any broadcast as a reason to sweep, and the bus carries
routine traffic — state snapshots, volume, availability. Events now only update
state; a sweep happens on the hourly tick, or on a transition back into
"allowed" (a session ending, the connection returning). Verified in the harness:
one sweep at startup, none during a session, and exactly one 30s after the
session ended.

No test would have caught this — it needed a real event bus with real traffic on
it, which is the argument for the headless run being part of the loop and not a
formality at the end.

### Testing

33 unit tests in `shepherd-media-cache` (keying, lock contention, store
classification, and the eviction-ordering cases: watched beating unwatched
however old, newest-unwatched first, least-recently-played first among watched,
prefetch recycling unwatched space, prefetch refusing to displace a watched
file, the `.played` marker outliving its video) plus 6 integration tests driving
the real worker over loopback HTTP. A test in `shepherdd` pins the prefetch
selector to the player's, since a mismatch would file every prefetched video
under a content key the player never looks up.

Verified live at a mocked `2026-08-22 14:00`: startup sweep queues the browse
library's one remote item, the direct-play entry queues nothing (its item is a
local file), the placeholder YouTube playlist in the example config warns once,
and the browse grid is unchanged.

### Note for a separate effort: a warning system for the management UIs

Phase 3 added three admin-facing warnings — missing yt-dlp, low disk, an
unreadable library — and they all go to the log, which is where an admin will
never look. The same is true of warnings that already exist elsewhere in
shepherdd (firewall not enforceable, browser policy ignored for a non-Chrome
kind, BLE bearer pinning unavailable).

A structured warning channel that surfaces in the companion app and web UI —
raised, cleared, with a severity and a suggested fix — would make these
actionable instead of archaeological. It wants its own issue: it touches the
wire schema, both UIs, and the question of what deserves to interrupt a parent
versus sit in a list.

## Converging the Android cache

> Follow-up prompt:
>
> > commit roughly by phase, then update the Android behavior to match
> > (refactoring if possible to share the ordering logic), then fix the Android
> > hashing (again, refactoring if possible to share the hashing logic)

The Android app keeps its own `video_cache.rs` — it downloads through Android
APIs, not yt-dlp — so none of the above reached it. Two things were worth
sharing rather than leaving divergent.

**The eviction ordering.** `Recency` and the `.played` marker moved into
`shepherd-media-app`, which both front-ends already depend on, as `lru::Recency`
and `interest`. Only the *policy* is shared: each cache still walks its own
directory with its own filters and bookkeeping. Android also stopped bumping
mtime inside `cached_path` — inspecting a cache is not using it — and now
records interest at the play site, and on an after-play `store`, for the same
self-eviction reason the Linux worker does.

The ordering is a no-op on Android today: `CacheMode::QueueAll` appears in the
settings picker but nothing acts on it, so everything that cache holds was
watched by construction. It stops being a no-op when that mode is wired, which
is worth knowing before wiring it.

**The naming.** Android named files by a `DefaultHasher` of the URL, commented
as a "stable hash". It is not: the standard library documents the output as
unspecified across releases, so a toolchain bump renames the whole cache at
once — everything re-downloads silently, and the orphans keep occupying the
size cap until eviction reaches them. This is the same trap the Linux side
avoided in phase 2 by reaching for SHA-256, and it surfaced only from checking
whether these changes affected Android at all.

`content_key` / `interest_key` moved into `shepherd-media-app::cache_key`, with
a test pinning the digests so a change to them is deliberate rather than an
accidental global cache invalidation. Android passes an empty selector: it only
caches direct HTTP, which involves no format selection, so it has one rendition
per URL and its content key doubles as its interest key.

Pre-existing files under the old naming are left alone on both platforms, on the
same reasoning: they are ordinary eviction candidates that age out, and
re-downloading them is what the next toolchain bump would have cost anyway.

**Verified on hardware (2026-08-22).** The commits above left the Android side
checked only by `cargo check`; it has since been validated on a Pixel 10a
(arm64-v8a, SDK 37) attached over USB.

*Build.* `cargo ndk -t arm64-v8a build -p shepherd-media-android` links the
cdylib (the `android-media` CI gate), `cargo ndk clippy` is clean under
`-D warnings`, and a full Gradle `assembleDebug` packages
`libshepherd_media_android.so` for **both** `arm64-v8a` and `armeabi-v7a` — the
32-bit ABI matters, the Fire TV stick is `armeabi-v7a` only.

*Unit tests on the device itself.* A cargo-ndk-built test binary runs straight
off `/data/local/tmp` — no APK, no signing, no emulator:

```sh
cargo ndk -t arm64-v8a test -p shepherd-media-app --no-run   # then push the
adb push target/aarch64-linux-android/debug/deps/<bin> /data/local/tmp/t
adb shell "mkdir -p /data/local/tmp/tt && TMPDIR=/data/local/tmp/tt /data/local/tmp/t"
```

**`TMPDIR` is not optional:** `std::env::temp_dir()` falls back to `/tmp`, which
does not exist on Android, so every test using `tempfile` or `temp_dir` fails
without it. For `shepherd-media-android` the binary also links the vendored
libmpv, so push `vendor/libmpv/<abi>/*.so` and set `LD_LIBRARY_PATH` to it.
81/81 `shepherd-media-app` and 8/8 `video_cache` tests pass this way — including
`cache_key::keys_are_pinned_to_known_values`, which is what proves the SHA-256
naming is identical on bionic/aarch64 and on the host.

One test had to be gated for this to work. The old
`youtube_is_unsupported_for_now` in `resolve.rs` passed on the host, where
`youtube::provider()` is `cfg`'d out and returns `None`, but compiled for
Android `provider()` returns the JNI binding, so `resolve` really ran it and
panicked in `ndk_context` ("android context was not
initialized") — a bare executable has no JVM and no Activity. The assertion was only ever about a platform *without* a provider, so
it is now `#[cfg(not(target_os = "android"))]` and renamed
`youtube_is_unsupported_without_a_provider`; the on-device suite is 28/28.

Nothing was changed in `run_jni` itself: android-activity initializes the
context at startup, so the app can never reach that panic, and `ndk-context`
0.1.1 offers no non-panicking accessor to guard with. Note also that no CI job
runs this crate's tests for Android — `android-media` only builds the cdylib —
so this was latent until the on-device workflow above exercised it.

*End-to-end in the app.* Driven against a loopback library over
`adb reverse tcp:8099 tcp:8099` (the app's `ureq` reaches the host at
`127.0.0.1`), with `settings.toml` written directly into
`/data/data/<pkg>/files` via `run-as` — which needs a **debug** build; the
release-signed APK is not debuggable, and its signature does not match the local
debug keystore, so swapping builds costs an `adb uninstall` and the user's
configured libraries with it. On a `queue-after-play` library:

| Step | Observed |
| --- | --- |
| First play (uncached) | streams; after EOF `store()` writes `629e0ed5eea189ecdce564b5d51ee9aa.mp4`, the exact host-computed `content_key(url, "")`, plus a `.played` marker under the same key |
| Replay | **zero** further HTTP requests — served from the cache file |
| Replay, `.mp4` mtime | unchanged — `cached_path` is a pure lookup, the 9a9ee36 fix, confirmed on real storage |
| Replay, `.played` mtime | moved forward — `mark_played` fires at the play site |
| Second video over a 160 KB cap | the older-played file is evicted; the newer stays; the cache lands within the cap |
| After that eviction | **both** `.played` markers survive, including the evicted file's — interest outliving any copy of the video, as documented |

The two-class ordering itself still cannot be exercised through the app, because
nothing on Android produces an unwatched file until `CacheMode::QueueAll` is
wired; it is covered by the on-device unit tests instead.

## Where issue #127 stands

All three phases are built. The kind is real, the cache is shared and correctly
keyed, and shepherdd prefetches.

Still open, and deliberately not in scope:

- **The stdout protocol reader** for playback-only time accounting, which
  `docs/shepherd-media.md` has always tracked separately. It is harder than it
  looks: `ManagedProcess::spawn` never pipes stdout, and the firewall path wraps
  the activity in `pkexec systemd-run --scope`, putting a privilege boundary
  between shepherdd and the stream.
- **Readiness gating for the media kind** (hiding tiles until `shepherd-media`
  and `yt-dlp` are present), which phase 3's warning makes visible but does not
  enforce.
- **The management-UI warning system** above.
- **The Android app**, which keeps its own cache and settings.
