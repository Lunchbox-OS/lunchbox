# A button to kick a media refetch (issue #165) — investigation

> Status: **built.** The investigation below is what was proposed; the four
> judgement calls were answered on 2026-09-05 and are recorded under
> "Decisions", and the two places the implementation improved on the plan are in
> "As built" at the end.

## Prompt

> investigate #165. This should be triggerable as a button in the BLE and
> Web-based management

and, after the investigation:

> build it: global, just ignore prefetch_while_session_active and RESUME_DELAY,
> yes it means "fetch it now", and use a diagnostic for a failed fetch

[Issue #165](https://git.armeafamily.com/albert/shepherd-launcher/issues/165):
*"Add a way to kick media refetch"* — body: *"This includes both the library and
the content itself"*; one comment: *"and sponsorblock"*.

## The problem, in hours

Everything on the media path is cached with a TTL and swept on a timer. Nothing
anywhere lets an administrator say "go and look now". The worst-case waits, all
measured from the code rather than from the docs:

| What | Where | Wait |
|---|---|---|
| YouTube playlist contents | `CACHE_TTL_SECS`, `shepherd-media-cache/src/playlist.rs:30` | 6 h |
| Prefetch sweep | `SWEEP_INTERVAL`, `shepherdd/src/media.rs:71` | 1 h |
| Retry after a failed download | `RETRY_COOLDOWN`, `shepherd-media-cache/src/download.rs:25` | 6 h |
| SponsorBlock segments | `DEFAULT_TTL`, `shepherd-media-app/src/sponsorblock.rs:32` | 24 h |
| Poster / thumbnail | `DEFAULT_TTL`, `shepherd-media-app/src/poster_cache.rs:24` | 6 h |

So: a parent adds a video to a YouTube playlist and it can be **6 hours** before
the child's browse grid shows it, **7** before it is cached for offline play, and
if that first download fails, another **6** before anything tries again. A
SponsorBlock segment submitted for a video already on the device is invisible for
up to a day. The only lever today is `reload_config`, which re-reads
`config.toml` and does not touch any of these caches.

That is the whole issue. It is three separate caches with three separate clocks
and no manual override on any of them.

## What "refetch" has to mean, cache by cache

All three caches are shared on disk between shepherdd's prefetcher and the
`shepherd-media` player process (`$XDG_CACHE_HOME/shepherd/media/<leaf>/`), so
invalidating from the daemon fixes the player too — that is what makes a
daemon-side button worth having at all.

- **Library.** For a playlist-backed library, the file
  `playlists/<list-id>.json` and its `fetched_at` field. For a file-backed
  library, nothing: `load_library` re-reads the `.toml` on every sweep and every
  launch, so it is already at most one sweep stale.
- **Content.** The video cache. Note carefully what is *not* stale: a committed
  `<key>.mp4` is keyed by URL + format selector, so the only way its bytes are
  wrong is a YouTube re-upload at the same URL. The thing that actually blocks
  content from arriving is the `<key>.failed` marker and its 6 h cooldown, plus
  the hour until the next sweep. Refetching content therefore means *clear the
  cooldowns and sweep now*, not *re-download 10 GiB*.
- **SponsorBlock.** The bucket files under `sponsorblock/<prefix>.json`, whose
  freshness is the file's own mtime.
- **Posters**, which the issue does not mention but which are the same class of
  staleness and the same 6 h TTL. Considered and dropped — see "As built".

### Never delete what you cannot replace

The obvious implementation — unlink the cache files — is wrong, and quietly so.
Every one of these caches has an offline stale-fallback: `cache::resolve` returns
`Resolution::Stale` when the live fetch fails and a cached copy exists, which is
what keeps thumbnails drawn and sponsors skipped on a device that has gone
offline. Deleting the file destroys that fallback, so a refresh pressed while the
network is flaky leaves the device *worse* than before it was pressed.

The investigation proposed **expire** — backdate the mtime, rewrite `fetched_at`
— to get staleness without removal. What shipped is simpler and gets the same
guarantee: **force the fetch**, write back on success, and on failure leave the
cached copy exactly where it was. See "As built".

`<key>.failed` is the exception — that one really is deleted, since the marker
*is* the cooldown and `store::clear_failed` already exists for it.

## Where the trigger goes

Cheap, because the RPC plumbing is generated. `#[management_rpc]` on the
`ManagementService` trait emits `dispatch_json`, and **both** transports route
through it (`shepherd-ble/src/rpc.rs`, `shepherd-http/src/handlers/rpc.rs`), so
one trait method is one BLE method and one HTTP method with no per-transport
code. `cargo run -p shepherd-wire-codegen --bin rpc-codegen` then regenerates
`docs/rpc-schema.json`, the TypeScript method/param/result maps, and the Kotlin
`RpcParams` builder; `tests/rpc_codegen_drift.rs` fails until they are committed.

`reload_config` is the exact precedent to copy — an administrator-triggered,
no-argument, `wrap_result` action with a button in both clients. Tracing it is
the whole checklist:

| Layer | File |
|---|---|
| trait + impl | `crates/shepherd-management/src/service.rs` |
| test double | `crates/shepherd-ble/src/testsupport.rs` (the only other impl) |
| generated | `docs/rpc-schema.json`, `shepherd-webui/src/api/rpc-methods.generated.ts`, `companion-android/.../RpcParams.generated.kt`, `.../ble/RpcMethods.kt` |
| web client | `shepherd-webui/src/api/client.ts` |
| web UI | `shepherd-webui/src/pages/AdminPage.tsx` |
| companion client | `.../domain/ManagementClient.kt` |
| companion UI | `.../ui/ShepherdViewModel.kt`, `.../ui/device/DeviceControlsScreen.kt` (the "Maintenance" card) |
| tests | `crates/shepherd-management/tests/dispatch.rs`, `crates/shepherd-http/tests/api.rs` |

### The one piece that is not free

`DefaultManagementService` has no way to reach the prefetcher. `MediaPrefetcher`
is owned by shepherdd, and its `run()` loop is a `select!` over an hourly ticker
and the event bus (`crates/shepherdd/src/media.rs:154`). It needs a third arm.

`shepherd-management` cannot depend on `shepherd-media-cache` — that would drag
the media stack into the transport crates. So the split is:

- the service holds an `Option<mpsc::Sender<MediaRefreshRequest>>`, sends, and
  returns;
- the prefetcher owns the receiver, and does the expiring *and* the sweep;
- `MediaRefreshRequest` (an `Option<EntryId>`) lives in `shepherd-api` beside the
  other wire-adjacent types.

`DefaultManagementService`'s fields are already all `pub` and it is constructed
in one place (`crates/shepherdd/src/main.rs:327`), so this is an added field and
a `mpsc::channel` at startup.

### It must not block the response

A manual sweep shells out to `yt-dlp` for every playlist. The companion's
`REQUEST_TIMEOUT_MS` is **15 s** (`ShepherdConnection.kt:653`); a playlist fetch
over a bad connection will beat that. So the RPC is fire-and-forget: it validates,
sends, and returns immediately with what it can know synchronously — how many
libraries it is about to refresh, read straight off the policy. Same shape as
`reload_config` returning `entry_count`:

```rust
#[rpc(wrap_result = "libraries")]
async fn refresh_media(&self, entry_id: Option<EntryId>) -> ManagementResult<usize>;
```

`NotFound` when `entry_id` names no entry, an unprocessable error when it names a
non-media one, and a plain error — not a silent success — when no channel is
wired, so an embedding without a prefetcher cannot answer "OK" to a button that
did nothing.

## Decisions (answered 2026-09-05)

**1. Per-entry, or the whole device? — Global.** No parameter at all:
`refresh_media()` takes nothing and covers every prefetch target. The video
cache and the segment buckets are one directory shared by every library anyway,
and what an administrator wants is "pick up what I changed", not "pick up what I
changed in this one place". `EntryView::kind_tag` still identifies media entries
if a per-entry button is ever wanted; adding one would not change the wire.

**2. Which policy gates does an explicit press override? — Exactly two.**
`prefetch_while_session_active` and the 30-second `RESUME_DELAY`, both of which
exist to keep *background* work off a device somebody is using. A press almost
always happens with the child in front of the device asking where the new video
is, which is precisely the state those two decline to work in.

Everything else stands, including the per-entry `prefetch = false` opt-out —
which means the refresh path reuses `PrefetchTarget` unchanged rather than
needing a second, metadata-only target list. The two device-wide gates that
survive (`service.media.prefetch = false`, and an offline device) are the ones a
button genuinely cannot overrule, and both now say so out loud rather than
returning silently; see decision 4.

**3. Does it re-download videos that are already cached? — No.** "The content
itself" means *fetch it now*, not *fetch it again*. So a refresh forgets the
download cooldowns and sweeps, and a `.done` file stays where it is. The content
key already covers every way a cached file can be the wrong bytes except a
same-URL re-upload, and a button that silently re-spends 10 GiB of a household's
bandwidth is not what "refresh" should mean on a kiosk.

**4. How does the parent know it worked? — A diagnostic**, rather than the new
event the investigation floated. It costs no new wire type, it is *state* rather
than a moment (which is what a condition like "the last refresh could not reach
YouTube" actually is), and both clients already render the set. New code
`media_refresh_failed`, raised on `Service` when the whole refresh could not
start and on `Entry` when one library fell back to cached data, cleared by the
next refresh that gets through.

A scheduled sweep deliberately touches that diagnostic in **neither**
direction. It reads from exactly the caches a refresh exists to bypass, so a
clean scheduled sweep is no evidence the source is reachable and must not clear
a standing complaint about it.

## As built

Two things came out better than the plan.

**The "expire, do not delete" primitive was not needed.** The investigation
proposed backdating mtimes and rewriting `fetched_at` so the caches would read
as stale while keeping their offline fallback. Forcing the fetch directly is
strictly better: `refetch_playlist` and `SponsorBlockCache::refresh` skip the
freshness check and write back on success, and on failure leave the cached copy
untouched. Same fallback preserved, one fewer concept, and no filesystem
timestamp manipulation. The property the investigation was protecting is now a
test in each store — *a failed refresh keeps the bucket it could not replace*.

**Posters were dropped.** They are the same class of staleness and were listed
as worth including, but the case does not survive contact: a new playlist item
has a *new* thumbnail URL, so it is a cache miss and fetched at the next launch
regardless. The only thing a poster refresh would fix is an image changing
behind an unchanged URL, and only the player fetches posters — at launch —
so the daemon could not act on it anyway.

One thing came out as planned but is worth writing down: the SponsorBlock
refresh has to **deduplicate by bucket prefix**. `warm` gets the second video in
a bucket for free because the first one's fetch wrote the file; a forced refresh
does not, so without a `HashSet` of prefixes a 92-item library would make 92
requests where a dozen would do.

### Shape of the change

| Layer | What |
|---|---|
| `shepherd-api` | `DiagnosticCode::MediaRefreshFailed` |
| `shepherd-media-app` | `BucketStore::refresh`, `prefix_for` — shared with Android |
| `shepherd-media-cache` | `refetch_playlist`, `SponsorBlockCache::refresh`, `store::clear_all_failures` |
| `shepherdd` | `SweepMode`, `MediaPrefetcher::refresh`, a third arm on the `select!`, the `mpsc` in `main.rs` |
| `shepherd-management` | `refresh_media()` on the trait, `media_refresh_tx` on the service |
| generated | schema + both clients' method/param/result mirrors |
| web UI | `refreshMedia()`, a Media card on the Admin page |
| companion | `ManagementClient.refreshMedia`, `ShepherdViewModel.refreshMedia`, a button in the Maintenance card |
| docs | `docs/shepherd-media.md` "Refreshing now", `shepherd-media-cache/README.md`, `shepherd-http/README.md` |

`refresh_media` returns `ManagementResult<()>` — no count. The service cannot
know how many libraries a refresh will cover without duplicating `read_policy`'s
gates, and a number that disagreed with what the prefetcher actually did would
be worse than no number.

## Verification plan

- `crates/shepherd-management/tests/dispatch.rs` for the RPC surface, the way
  `reload_config_valid_file_wraps_entry_count` does.
- Unit tests in `crates/shepherdd/src/media.rs` for the expiry: that an expired
  entry re-fetches, and — the point of choosing expire over delete — that a
  refresh followed by a failed fetch still serves the stale copy.
- Web end-to-end through the `headless-dev` skill (`./scripts/shepherd dev
  headless` → `dev shot` → `dev stop`).
- BLE end-to-end through the `companion-pairing` skill, against the headless
  session over adb.
