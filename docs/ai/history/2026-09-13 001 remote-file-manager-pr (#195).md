# Remote file manager (#195)

Manage this device's files from the web interface: browse every place it can
reach, upload into them, and rename, move and delete what is there — over a
wifi chip that drops, without losing a four-gigabyte upload to it.

Closes #195.

## Why

A hardened kiosk account denies SSH and keeps its home at mode 0700, so `scp`,
`rsync` and every SFTP client are shut out of exactly the directory a book, a
ROM or a video has to go into. The workaround — copy in as another user, then
`sudo mv` — leaves root-owned files in a child's home.

Three existing diagnostics have no remedy except getting a file onto the
device: `EbookBookMissing`, `RetroarchContentMissing` and
`MediaLibraryUnreadable`. Since #185 the config editor can write the policy
that *names* a book; nothing could put the book there.

`shepherdd` already runs as the kiosk user, so it is the one process on the
device for which none of this is a problem.

## What it looks like

A **Files** tab, laid out like the list view in the Finder: the device's places
are the top level of one tree and a folder opens in place, so a device with a
home directory and two removable drives is still one screen with no breadcrumb
and no back button — and a parent can see both ends of a copy at once.

- Sortable columns (name, size, kind, modified), folders on top, hidden files
  behind a toggle.
- Upload by button, by the ⋮ menu, or by dropping files straight onto a folder
  row. A transfer tray shows progress, survives switching tabs, and picks a
  dropped transfer back up where the device got to rather than starting the
  file again.
- New folder, rename in place, delete, and move by dragging a row onto a
  folder — with **Move to…** in the ⋮ menu for anyone who cannot drag. A folder
  hovered mid-drag springs open.
- Below `sm` the columns fold into a second line under the name, and every
  gesture has a menu equivalent, because a phone has no drag and a screen
  reader has no drop.

## The shape of the API

Seven paths under `/api/v1/files`, inside the existing `require_auth` layer,
so everything #156 built applies unchanged: session cookie or bearer token, the
`Origin`-vs-`Host` check on writes, lockout, TLS, revocation.

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/files/roots` | The places a caller may browse, plus the upload limits |
| `GET` | `/files/list` | One directory |
| `GET` | `/files/content` | Download (`HEAD`, `Range` and `If-Range` too) |
| `PUT` | `/files/content` | Upload or replace; in pieces with `Content-Range` |
| `GET`/`DELETE` | `/files/upload` | What survives of an interrupted upload, and giving up on one |
| `POST` | `/files/dir` | Create a directory |
| `POST` | `/files/move` | Rename or move, within one root |
| `DELETE` | `/files/entry` | Delete a file or directory |

**A caller names a root the server enumerated and a path inside it, never an
absolute path.** That is the `ProtectedFile`-is-an-enum argument applied as far
as an API whose whole job is naming files can apply it: a closed set the server
offers cannot express a location it did not offer. Roots are the kiosk home,
every removable drive under `/media` or `/run/media` (keyed on its **filesystem
UUID**, so a drive keeps its identity across a replug and two drives labelled
`UNTITLED` do not collide), and each configured `extra_roots`. Both travel as
query parameters, because a wildcard path segment is percent-decoded by the
router before any check sees it — which is how `%2e%2e%2f` becomes `../` one
layer too early.

**No runtime toggle, and nothing on `ManagementService`.** A caller who reaches
these routes can already `PUT /api/v1/config` with `kind = { type = "process",
command = … }`, which runs anything as this user, so withholding a file write
from the same credential protects nothing — the argument `App.tsx` already
records for the config editor. And `#[management_rpc]` carries every async
trait method to BLE, whose frames cap at 16 KiB, so a file transfer there would
be a method that exists and cannot work. The companion gets nothing from this
and needs nothing.

## What it will not do

- **Escape a root.** One resolver sees every caller-supplied path: `..` is
  refused rather than normalised, the parent is canonicalised before the check,
  and a symlink out of the root is *listed* — so a person can see and delete
  it — but never followed. Any activity at the kiosk uid can plant one, so this
  is not hypothetical. There is a table-driven escape suite over it.
- **Serve shepherd's own state, or an SSH key.** The data directory *this
  process actually opened* (so a `shepherdd -d /srv/library` inside the home is
  refused by the path it really has, not by the one the environment suggests),
  `$XDG_CACHE_HOME/shepherd` (the video cache, whose index hand-deletion
  desynchronises) and `~/.ssh` are refused for reading and writing alike.
  `~/.local/state/shepherdd` is deliberately *not*: pulling `shepherdd.log` off
  a device with no shell is one of the better things this buys.
- **Let a browser guess what an API answer is.** Every response under
  `/api/v1` carries `X-Content-Type-Options: nosniff`, from one layer rather
  than a line per handler — errors and the unknown-path fallback included. The
  bodies are `application/json`, which no browser renders as HTML, but several
  of them carry text the *caller* chose (a filename in a listing, the path an
  upload echoes back) and `serde_json` escapes quotes and control characters,
  not `<`, `>` or `&`. That left the content type as the only thing between a
  filename and a browser reading it as markup — and this router has shipped the
  matching bug once already, when unknown `/api/v1` paths reached the SPA
  fallback and answered `200 text/html`. Error messages also no longer quote a
  path component back at the caller, which told them nothing they had not just
  sent. A filename carrying a quote and a CRLF reaches `Content-Disposition`
  percent-encoded, so there is no header injection either.
- **Run an uploaded file as this origin.** Downloads are always
  `Content-Disposition: attachment` + `X-Content-Type-Options: nosniff` + a
  fixed `application/octet-stream`. An uploaded `.html` served inline would run
  script on the management origin, where `fetch('/api/v1/rpc')` carries the
  administrator's cookie — `HttpOnly` does not help, because such a script
  never reads the cookie, it is merely sent with it. The SPA's own responses now
  carry a `Content-Security-Policy` for the same reason (which also required
  `'wasm-unsafe-eval'`, without which the config editor's validator will not
  load).
- **Trust a tag the filesystem cannot keep.** FAT and exFAT record mtimes at
  two-second resolution, and the etag is `size-mtime`, so two different files
  of the same size written in the same tick carry the same tag. A file younger
  than one tick on such a filesystem is tagged **weakly** (`W/"…"`), which
  brings HTTP's own rules with it: a weak validator cannot assemble a range,
  never satisfies an `If-Match`, and is not answered with `304`. Past the tick
  it is strong again, so the steady state is unchanged. Found by mounting a
  real FAT image.
- **Let two creators both win.** `If-None-Match: *` is checked before the body
  arrives — long enough for another administrator, or this caller's own retry,
  to take the name. The promise is kept at the rename instead, with
  `RENAME_NOREPLACE`, so the loser gets a `412` rather than a `201` over
  somebody else's file.
- **Stitch two versions of a file together.** A browser resuming an
  interrupted download sends the validator it started with — Firefox as
  `If-Match`, Chrome as `If-Range` — and both are honoured, so a file that
  changed in the meantime is refused (`412`) or sent whole rather than
  continued from the old offset. Measured against a real Firefox: before this,
  the resumed download was half one version and half another and was reported
  as a success.
- **Lose a big upload to a bad wifi chip.** Anything over 8 MiB goes up in
  pieces (`Content-Range` + an `upload` token), so the unit of retry is a chunk
  rather than a file; a resumed transfer asks the device where it got to and
  carries on from *there*, which matters because a connection that dies
  mid-chunk leaves a partial one behind. Transient failures retry with backoff,
  a request that has not moved a byte in thirty seconds is aborted and retried
  rather than waited on, and the tray's Retry resumes rather than restarts.
  `GET`/`DELETE /files/upload` report and discard what survives.
- **Clobber quietly.** A precondition is required on `PUT` *and* `DELETE`:
  `If-None-Match: *` to create, `If-Match: "<etag>"` to replace the version you
  read, `If-Match: *` for whatever is there, and `428` without one. An upload
  always tries to create first, and a `412` becomes a *question* in the tray —
  a Replace button that re-sends with `If-Match: *` — rather than an error.
- **Half-write a file.** An upload streams to a dotted `.part` file in the
  destination directory, is `fsync`ed, and is renamed into place, so a child
  mid-book never opens a partial one. That part file is also the resumption
  state, so there is no session table and a restart loses only the chunk in
  flight. A resumable upload's precondition is checked twice — at the first
  chunk and again at the rename — so a file that appeared while a 4 GiB upload
  was in flight is not overwritten by it.
- **Fill the disk.** `max_upload_bytes` and `free_space_floor_bytes` are checked
  against `Content-Length` before the first byte and again as the stream grows,
  because a declared length can be a lie — and again in the browser, so an
  oversized file never leaves the page. The free-space floor guards the
  device's own disk only: a removable drive may be filled to the last byte,
  because a full USB stick costs nobody a session and a 2 GiB floor made every
  drive smaller than that unwritable.
- **Report a drive's refusal as a fault in the device.** A name FAT cannot
  spell, a file past FAT32's 4 GiB ceiling, a read-only mount and an over-long
  name were all `500 internal`; they are `400`, `413`, `403` and `400`, each
  saying what to do about it. A `500` tells a person their device is broken.
- **Move between places.** `POST /files/move` takes one root and cannot express
  it: `rename(2)` does not cross filesystems. Refused explicitly rather than
  half-implemented.
- **Take a dropped folder.** They arrive as zero-byte entries that would upload
  as empty files of the same name and look like it worked. Refused with a
  sentence; the recursive walk they need is its own feature.

## Configuration

```toml
[service.file_manager]
# enabled = true                        # false leaves the routes unmounted
# max_upload_bytes = 8589934592         # 8 GiB; 0 removes the cap
# free_space_floor_bytes = 2147483648   # 2 GiB, on this device's own disk only
# external_media = true                 # drives under /media and /run/media
# [[service.file_manager.extra_roots]]
# label = "Shared videos"
# path = "/srv/media"
```

`extra_roots` paths are validated at parse time: absolute, and neither being
nor containing `/`, `/etc`, `/var/lib/shepherdd` or the other system
directories. Containment is the case that matters — `/var` is an
ordinary-looking path that hands out the policy, the admin record and the web
credentials.

The table is also wired into the config editor, because
`npm run check:coverage` fails on a generated schema field the editor cannot
set.

## Notable decisions

- **Intent lives in a reducer, facts in the query cache.** Which folders are
  open, how the columns are sorted, what is selected — none of it is ever
  invalidated by a refetch; one query per directory holds the contents. That is
  why a folder refetched after a write re-renders in place instead of
  collapsing, and it makes the visible row list a pure walk that a windowing
  wrapper could take over later without a rewrite.
- **`useQueries` with `combine`, and paging inside the client.** The flat row
  list means the number of open folders changes between renders, which rules
  out a hook per node and therefore `useInfiniteQuery`. `listDirectory` walks
  the cursor itself up to a bound, and `truncated` keeps meaning exactly what
  the row at the bottom of the folder says.
- **Sorting is the client's, except in a folder that stopped short.** The API's
  cursor is a position in *its* order, so re-sorting half a folder by size
  would show a page boundary that makes no sense. Those keep the server's order
  and say so.
- **Permissions are a tested module.** Two things are easy to get backwards:
  deleting an entry is a permission on its *parent*, and an escaping symlink is
  deletable — which is the entire reason it is listed rather than hidden. They
  decide whether each control is drawn at all, because a menu item that 403s is
  worse than one that is not there.
- **The event stream no longer invalidates these queries.** Nothing on it
  describes the filesystem, so an open tree would have refetched every folder
  on every volume nudge.
- **Not every 5xx is retried.** `507 Insufficient Storage` is a decision — the
  disk is full, and re-sending gigabytes will not change that — so only 500,
  502, 503 and 504 are treated as "ask again". Writing the test is what made
  that obvious.
- **Unknown `/api/v1` paths now answer a JSON 404.** They previously fell
  through to the SPA and answered `200 text/html`, which an API client parses
  as JSON and fails on somewhere else entirely. It is also what a device with
  `enabled = false` returns.

## Testing

Five layers, because the bugs turned out to live between them.

- **Unit** — 25 tests over path resolution and the escape table, the validator
  arithmetic, the errno mapping, and `/proc/mounts` parsing (a label with a
  space, a filesystem with no UUID, two sticks both labelled `UNTITLED`, a torn
  line).
- **The wire contract** — 50 route tests in
  `crates/shepherd-http/tests/files.rs`: preconditions on every write, the
  denied subtrees, read-only places, the `attachment` headers, `Range`, the
  resumable protocol, and one asserting none of this reached the RPC surface.
- **The filesystem underneath it** — 11 tests in `files_on_disk.rs`, for what a
  protocol test cannot see: a filename that is not UTF-8, a fifo (where a route
  that opened before it asked questions would block a worker thread forever —
  the test asserts an answer *arrives*, not just that it is a `400`), a
  recursive delete with a symlink out of the root inside it, the 24-hour
  `.part` sweep, two writers at once, a reader while a writer renames, and a
  24 MiB body both in one piece and in chunks. The concurrency tests assert
  invariants that hold in every interleaving, so they are deterministic even
  though what happens is not.
- **A real removable drive** — 8 tests in `files_removable.rs`, against two
  loopback FAT32 images that `scripts/integration-tests/setup-removable-dev.sh`
  mounts under `/media` (512 MiB, so one is under the free-space floor; the
  second read-only). `#[ignore]`d and printing `[SKIP]` without them, so
  `cargo test --include-ignored` is the same command on CI.
- **A real daemon** — 5 tests in `crates/shepherd-e2e/tests/files.rs`: a 3 MiB
  file up and back down byte-identically through a real socket, a resumable
  upload spanning three connections, `enabled = false` leaving no routes, the
  daemon refusing its own state directory — and the routes answering `401`
  without a credential, which the in-process tests cannot see, because their
  router is built without a credential store.

**Web**: 204 tests. The walk, the sort, the subtree pruning, the drop parsing
and the move rule are pure; the mount tests cover what only exists across a
mount — opening a folder *fetches* it, closing one stops asking, a `412`
renders the conflict, a read-only place offers nothing that would fail.
`useUploads.test.tsx` covers the bad link in particular. New in this round:
`src/api/files.test.ts`, 17 tests pinning the shape of each request — the page
loop and its budget, the three preconditions, inclusive chunk ranges with a
short last chunk, the upload token's alphabet, both download paths, and query
encoding round-tripping a path with `&`, `#` and a space. That layer had no
test, and it is where a mistake is silent: a precondition header spelt wrongly
does not throw, it clobbers a file.

`cargo fmt --check` clean, zero clippy warnings, 93 test binaries green.

### Measured rather than assumed

- **Resumption.** 30 MB uploaded with the connection killed twice mid-chunk:
  the device reported offsets of 2,097,152 and 12,582,912 — neither a chunk
  boundary — the transfer carried on from each, and the file landed
  byte-identical, through `curl` and through the UI. Abandoning an upload took
  its 12 MB part file with it, and a token of `../escape` was refused.
- **Downloads, through real browsers.** A 40 MB download in Firefox, cut
  mid-body by a logging proxy, then resumed through the browser's own download
  manager: `Range` + `If-Match`, answered `206`, landing byte-identical. With
  the file changed underneath it, `412` — and Firefox discarded the partial
  data rather than finishing a corrupt file. Chrome 153 (the Flatpak this
  project installs) resumes by itself with `Range` + `If-Range`, byte-identical,
  and discards its partial data when the file changes. `curl -C -` too.
- **On a device**, not only in jsdom: `shepherd dev headless` with a second root
  configured, driven through geckodriver. New folder, upload, rename, delete,
  drag-to-move, the spring-loaded folder asserted *before* the release, and
  **Move to…** — each checked against the filesystem afterwards rather than
  only against the page. Then a `curl` sweep of the refusals.

### What that found

Seven bugs, in a feature whose tests were all green — every one of them in the
gap between "the protocol is right" and "the storage under it behaves the way
the protocol assumes". A stale `If-Match` on a FAT drive answered `206` with
bytes from a different file; the free-space floor made every USB stick under
2 GiB read-only; four of a drive's ordinary refusals read as `500`; the upload
response handed out a strong tag it could not keep; a `412` said "that file has
changed" about a file nobody had touched; two creators of the same name both
got a `201`; and the refused data directory was an environment guess rather
than the one the daemon opened. All fixed here, each with the test that found
it. The audit and the ten gaps are in
`docs/ai/history/2026-09-14 004 closing-the-file-handling-test-gaps (#195).md`.

## Reviewing this

Fourteen commits in four groups: the API and its design (1–3), the UI (4–6),
the resilience work (7–8), and the testing and hardening round that followed
(9–14). The seams are clean between them. In order:

1. `feat(http): manage this device's files from the web interface` — the
   routes, the resolver, the root enumeration, the config.
2. `docs(files): component architecture for the file manager UI` — the design
   the UI commits follow.
3. `feat(http): tell a client what it may do before it tries` — three gaps the
   UI design found: `unusable: <reason>` instead of a bare flag, `EINVAL` on a
   self-nesting move as a `400` rather than a `500`, and per-directory
   `writable`.
4. `feat(webui): a Finder-style file tree, read-only to start`.
5. `feat(webui): upload, rename, new folder and delete in the file tree`.
6. `feat(webui): move a file by dragging it onto a folder`.
7. `feat(files): uploads that survive a bad link` — resumable chunked uploads,
   retry with backoff, a stall watchdog, and `If-Range` on downloads. Prompted
   by review: the protocol had no answer for the wifi chip on repurposed
   hardware, where a drop at 95% of a 4 GiB video cost 4 GiB.
8. `fix(http): honour If-Match when a browser resumes a download` — what a
   real Firefox turned out to send. Small, and the one commit here that fixes
   silent corruption rather than adding a capability.
9. `docs(files): scope diff-based uploads before building them` — a survey, not
   an implementation. Recommends against the rsync rolling hash and finds that
   the everyday win is blocked by one header this branch does not have.
10. `fix(files): three bugs a real FAT drive found` — weak validators, the
    free-space floor, and a drive's refusals. **The commit to read closely.**
11. `test(files): run the file routes against a real FAT drive` — the loopback
    harness, plus the two more bugs its first run found.
12. `test(files): what a protocol test cannot see` — odd files, concurrency,
    size. Found the `If-None-Match` race.
13. `test(webui): pin the file manager's transport` and
    `test(e2e): the file manager against a running daemon` — the two ends of
    the wire. The second found the data-directory gap.
14. `fix(http): nosniff on every API answer, and stop quoting the caller` —
    prompted by a review question about the error path. No injection, but the
    margin was one unasserted header wide; now it is a layer with tests that
    were checked to fail without it.

The reasoning behind each stage, including what was rejected, is in
`docs/ai/history/2026-09-11 003…005`, `2026-09-13 002` and
`2026-09-14 001…004`.

## Follow-ups, deliberately not here

- Dropping a *folder* from the desktop (recursive walk, `mkdir` per level, a
  resumption story).
- Multi-select, and therefore multi-delete and multi-drag.
- Previews and thumbnails — they trade directly against the three download
  headers, and want their own issue now that there is a CSP to lean on.
- Copy, and a zip download.
- A "fix this" affordance from the Diagnostics page: `EbookBookMissing` names a
  path, and the tree could open at its folder with the upload button primed.
  That is what turns a file manager into a setup tool.

And three limits left in the resilience work on purpose:

- **Resuming across a page reload needs the file re-added.** The device side
  resumes — the token is derived from the file, so re-adding it continues what
  is there — but a browser cannot re-read a `File` handle from a previous
  session without the Chromium-only File System Access API.
- **No server-side inactivity timeout.** The client's watchdog covers the case
  a person is watching; a stalled connection the client has forgotten holds its
  part file until the sweep reaches that folder. The sweep is one directory
  deep and opportunistic — it runs where an upload lands and where somebody is
  looking, not on a clock — so a folder that is never uploaded to or opened
  again keeps its stray bytes. They are listed as a hidden entry, so a person
  can delete them.
- **Cross-origin downloads still buffer to a blob**, so they neither resume nor
  stream. That is the `apiBase`-points-elsewhere path, which needs it least.
- **`RENAME_NOREPLACE` has no equivalent on vfat**, so two simultaneous
  creators of the same name on a USB stick can still both answer `201`. Closed
  where it matters — the device's own disk, which is where two administrators
  are actually both writing — and left open where the filesystem offers nothing
  better.

---

Unrelated, and noticed while verifying:
`shepherd-host-linux`'s `retroarch_spawn_materializes_config_and_argv` fails
about one run in four, on `main` as well as here. It passes in isolation, so it
is an ordering or parallelism dependency in that suite rather than anything in
this branch.
