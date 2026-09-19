# Remote file manager (issue #195) — API design

> Status: **built** (server side), 2026-09-11. Follows
> `2026-09-11 003 remote-file-manager (#195).md`, whose five questions were
> answered on 2026-09-11 and are recorded there. The body below is the wire
> contract as it was put up for review; where the review changed it, and where
> building it changed it, is recorded under "What was built" at the end.

## Prompt

> 1: no toggle. 2: ~. 3: autodetect external drives at /media/... and
> configurable additional directories. 4: out of scope for now. 5: nothing in
> the companion. Design the API before proceeding further

## What the decisions settle before the first route

- **No toggle.** The surface is always there for an authenticated
  administrator, exactly like the config editor. Nothing to turn on, no expiry,
  no state to persist, no event to broadcast, no HUD indicator.
- **Nothing in the companion.** So nothing goes on `ManagementService`. These
  are HTTP routes only, the same shape as `/api/v1/config` — and with the
  companion gone, so is the last reason to put even a settings read on the
  trait. The macro, the codegen, `docs/rpc-schema.json`, `WireTypes.generated.kt`
  and `rpc-methods.generated.ts` are all untouched by this feature.
- **Root is the kiosk home**, plus removable drives, plus configured extras.
  That makes roots a *set* rather than a constant, which is the single biggest
  influence on the shape below.
- **No in-place editing.** No `text/plain` round trip, no editor integration.
  Download, edit, upload.

## The one decision the wire shape turns on

**A caller names a root and a path inside it. It never names an absolute
path.**

The rejected alternative was to take an absolute path and check it is beneath
some allowed root. It is simpler by one indirection and it is the wrong
default here, for the reason `ProtectedFile` is an enum rather than a path
(`crates/shepherd-util/src/paths.rs:250`): a closed set the *server* enumerates
cannot express a location the server did not offer, whereas a validated string
is one missed call site away from serving `/etc`. Roots are enumerated by
`GET /api/v1/files/roots`; every other route takes `root` = one of those ids.

It also keeps the device's layout out of the client. The SPA renders
`Home › Books › covers`, not `/home/kiosk/Books/covers`, and a breadcrumb
cannot walk above its root because there is no spelling for it.

```
root = "home"            an id from /roots, opaque to the client
path = "Books/covers"    relative, "/"-separated, "" is the root itself
```

Both travel as **query parameters**, not path segments. A wildcard path
segment would be percent-decoded by the router before our check sees it, which
is the classic way `%2e%2e%2f` becomes `../` one layer too early; query
parameters are decoded exactly once, by `serde_urlencoded`, into a string we
then validate ourselves. `POST` and `DELETE` bodies use JSON with the same two
fields.

## Routes

All under `/api/v1/files`, all inside the existing `guarded` router in
`handlers/mod.rs` — so all of #156 applies unchanged: session cookie or bearer,
the `Origin`-vs-`Host` check on writes, lockout, TLS, revocation.

| Method | Path | Purpose |
|---|---|---|
| `GET` | `/api/v1/files/roots` | The places a caller may browse, and the limits |
| `GET` | `/api/v1/files/list` | One directory |
| `GET`/`HEAD` | `/api/v1/files/content` | Download a file |
| `PUT` | `/api/v1/files/content` | Upload or replace a file |
| `POST` | `/api/v1/files/dir` | Create a directory |
| `POST` | `/api/v1/files/move` | Rename or move, within one root |
| `DELETE` | `/api/v1/files/entry` | Delete a file or directory |

`DELETE` gets its own noun rather than sharing `/content` because it deletes
directories too, and a route whose meaning depends on what it finds on disk is
a route that will one day delete the wrong kind of thing.

### `GET /api/v1/files/roots`

```json
{
  "roots": [
    {
      "id": "home",
      "label": "Home",
      "kind": "home",
      "path": "/home/kiosk",
      "writable": true,
      "total_bytes": 494384795648,
      "free_bytes": 201234567890
    },
    {
      "id": "ext-9f2a1c04",
      "label": "KINGSTON",
      "kind": "external",
      "path": "/media/kiosk/KINGSTON",
      "writable": true,
      "total_bytes": 61440000000,
      "free_bytes": 42000000000
    },
    {
      "id": "extra-0",
      "label": "Shared videos",
      "kind": "configured",
      "path": "/srv/media",
      "writable": false,
      "total_bytes": 2000398934016,
      "free_bytes": 154000000000
    }
  ],
  "limits": {
    "max_upload_bytes": 8589934592,
    "free_space_floor_bytes": 2147483648
  }
}
```

- `id` is stable for `home` and for configured extras (`extra-<index>`);
  for a removable drive it is `ext-` plus the first four bytes of
  SHA-256 over the mount point, because a drive's *label* can repeat and its
  mount point cannot. A drive unplugged mid-session takes its id with it and
  every later call answers `404 not_found` — which is the honest answer, and
  what the UI needs to send the user back to the roots list.
- `path` is display-only. Nothing accepts it back.
- `writable` is `access(W_OK)` on the root itself. An externally mounted drive
  owned by root reads `false`, and the UI hides upload and delete rather than
  offering buttons that will 403.
- `total_bytes` / `free_bytes` come from `statvfs`, via the same helper
  `shepherdd`'s media sweep uses (`crates/shepherdd/src/media.rs:800`, which
  already walks up to an existing ancestor). Moving it to `shepherd-util` is
  part of this work.
- `limits` is here rather than on its own endpoint so the UI can size its
  upload guard from the same response that drew the sidebar.

**Removable drives are detected from `/proc/mounts`**, taking every mount point
directly under `/media/` (any user's subdirectory — `udisks2` mounts at
`/media/<user>/<label>`) and under `/run/media/`. Not from `udisks2` over
D-Bus: a sway kiosk has no automounter running in the common case, the drive
is mounted by the administrator or by `fstab`, and reading `/proc/mounts` sees
all of those identically. Pseudo-filesystems and anything not `access(R_OK)`
are dropped.

**Re-read per request.** A drive plugged in while the page is open appears on
the next `roots` call; the SPA refetches on focus and after an error. No
inotify, no mount-table watcher, no cached root list — the whole cost is one
`/proc/mounts` read and one `statvfs` per root.

### `GET /api/v1/files/list?root=home&path=Books&limit=1000&cursor=…`

```json
{
  "root": "home",
  "path": "Books",
  "writable": true,
  "entries": [
    { "name": "covers", "kind": "dir",  "size": null, "modified": "2026-09-01T12:00:00-04:00", "etag": null, "hidden": false, "symlink": false, "writable": true },
    { "name": "the-hobbit.epub", "kind": "file", "size": 1863410, "modified": "2026-08-30T09:12:44-04:00", "etag": "1863410-1756557164123456789", "hidden": false, "symlink": false }
  ],
  "truncated": false,
  "cursor": null
}
```

- **Order is the server's**: directories first, then files, each by
  case-insensitive name. It has to be, because the cursor is a position in that
  order; a client that re-sorted a paginated listing would show a jumbled page
  boundary. `cursor` is opaque (the last entry's sort key) and only appears
  when `truncated` is true. `limit` defaults to 1000 and caps at 5000 — a ROM
  set in one directory is a real thing and a 50 000-entry JSON response is not.
- **`hidden` is a flag, not a filter.** Dotfiles are always returned and the
  SPA hides them behind a toggle, because `~/.config/shepherd/movies.toml` is a
  file a parent genuinely edits and a server-side filter would mean a second
  round trip to reveal it.
- **`symlink: true` means the entry is a symlink**, and `kind` describes what
  it points at *only if the target is still inside the root*. A link whose
  target escapes is reported as `kind: "file"`, `usable: false` — it is listed,
  so a person can see and delete it, and it is not traversable or readable.
  This is the one place the escape check is visible in the wire format, and it
  is deliberate: a kiosk user (and therefore any activity, which runs at the
  same uid) can create such a link at any time.
- **`usable: false` also covers a name that is not valid UTF-8.** Linux permits
  arbitrary bytes; `name` is then lossy-decoded for display and every operation
  on it is refused, because a lossy name cannot be turned back into the bytes
  that would address the right file. Nothing uploaded through this API can ever
  be in that state — it only describes what was already on the disk.
- **`etag` is `"<size>-<mtime_nanos>"`**, not a content hash. This is a
  deliberate departure from `PolicyDocument::version_of`, which hashes because a
  policy is tens of kilobytes and a restored-from-backup file should read as
  unchanged. Hashing a 700 MB ROM on every directory listing is not the same
  trade. The tag is opaque, compared for equality, and documented as such.
- `modified` is RFC 3339 with the device's offset, like every other timestamp
  on this API.

### `GET`/`HEAD /api/v1/files/content?root=home&path=Books/the-hobbit.epub`

Streams the file. Headers, and every one of them is load-bearing:

```
Content-Type: application/octet-stream
Content-Disposition: attachment; filename*=UTF-8''the-hobbit.epub
X-Content-Type-Options: nosniff
Cache-Control: no-store
Accept-Ranges: bytes
ETag: "1863410-1756557164123456789"
```

**Never a guessed content type, and always `attachment`.** An uploaded
`.html` served inline from this origin runs script on the management origin,
where `fetch('/api/v1/rpc', {credentials:'include'})` is the whole API with the
administrator's cookie attached — `HttpOnly` does not help, because the script
never needs to read the cookie, only to be sent with it. `attachment` +
`nosniff` + a fixed type is what stops that, and no preview or thumbnail
feature may weaken any of the three (see "Deferred" below).

`Range` is honoured for a single range (`206` + `Content-Range`); a
multi-range request gets the whole file with `200`, which the spec allows and
which nothing we ship will ever send. `If-None-Match` answers `304`.
`HEAD` returns the same headers with no body, so the UI can show a size before
committing to a download.

### `PUT /api/v1/files/content?root=home&path=Books/the-hobbit.epub`

Raw body, `Content-Type` ignored. Not `multipart/form-data`: a raw body
streams without a parser, needs no new axum feature, and is what the config
editor's `PUT` already does.

**A precondition is required**, mirroring `/api/v1/config` and for the same
reason — this device has more than one writer, and a forgotten header should be
a refusal rather than a clobber:

| Header | Means |
|---|---|
| `If-None-Match: *` | Create. `412` if anything is already there. |
| `If-Match: "<etag>"` | Replace exactly that version. `412` if it changed. |
| `If-Match: *` | Replace whatever is there. |
| *(none)* | `428 precondition_required`, with a message naming the three. |

Responses: `201` on create, `200` on replace, both with the new `ETag` and
`{"path": "...", "size": 1863410, "etag": "..."}`.

Mechanics that belong in the contract because they are observable:

- The body streams to `.<name>.<random>.part` **in the destination directory**,
  is `fsync`ed, and is `rename`d into place. Same directory so the rename is
  atomic; a reader never sees a half file, and a child mid-book never opens
  one. A dropped connection leaves the `.part` file, which is swept on the next
  write to that directory and is why the name is dotted.
- `413 too_large` if `Content-Length` exceeds `max_upload_bytes`, and again —
  mid-stream, connection dropped — if the declared length was a lie.
- `507 insufficient_storage` if accepting the body would take the filesystem
  below `free_space_floor_bytes`. Checked before the first byte against
  `Content-Length`, and again as the stream grows. A kiosk with a full disk is
  a session that will not start, which is why this is a refusal and not a
  warning.
- The parent directory must exist. `PUT` does not create it; `POST /dir` does.
  Implicit directory creation from a path typo is how you get `Boks/`.

### `POST /api/v1/files/dir`

```json
{ "root": "home", "path": "Books/covers" }
```

`mkdir -p` semantics, every created component checked the same way. `200` if
it was already a directory (idempotent, so a retry after a dropped response is
safe), `409 conflict` if a file is in the way, `201` on create.

### `POST /api/v1/files/move`

```json
{ "root": "home", "from": "Downloads/hobbit.epub", "to": "Books/the-hobbit.epub", "overwrite": false }
```

Rename within one root. `overwrite` defaults to `false` → `409` if the target
exists; `true` replaces atomically. **Cross-root moves are refused**
(`400 bad_request`, with a message saying to download and re-upload): `rename(2)`
does not cross filesystems, and a copy-then-delete loop with progress,
resumption and partial-failure semantics is a feature of its own, not a footnote
in this one.

### `DELETE /api/v1/files/entry?root=home&path=Books/old.epub&recursive=false`

`204` on success. A non-empty directory without `recursive=true` is `409`. A
root itself is `403` — a button that empties the home directory is not a button.

`If-Match` is **honoured but not required** here, unlike `PUT`. The asymmetry
is deliberate: an overwrite is a silent loss of someone else's work, while a
delete is an explicit act behind a confirmation dialog, and requiring a
precondition on it would mean a `stat` round trip before every delete in a
multi-select.

Recursive delete does not follow symlinks — it unlinks them, like
`remove_dir_all` does — so a link into `/` deletes the link, not the system.
This is worth a test rather than a comment.

## Errors

The existing envelope, unchanged: `{"error": "<code>", "message": "<human>"}`.

| Status | `error` | When |
|---|---|---|
| 400 | `bad_request` | Malformed path, `..`, absolute, cross-root move, bad cursor |
| 403 | `forbidden` | Escapes the root, a denied directory, `EACCES`, deleting a root |
| 404 | `not_found` | Unknown root (including an unplugged drive), missing path |
| 409 | `conflict` | Target exists, non-empty directory, file where a directory was asked for |
| 412 | `precondition_failed` | `If-Match` / `If-None-Match` did not hold |
| 413 | `too_large` | Over `max_upload_bytes` |
| 428 | `precondition_required` | `PUT` with no precondition |
| 416 | `range_not_satisfiable` | Bad `Range` |
| 507 | `insufficient_storage` | Would cross the free-space floor |
| 500 | `internal` | Anything else, with the `io::Error` in the message |

These routes do **not** go through `ManagementError`. That enum's `Conflict`
maps to `412` in `handlers/config.rs` because a policy write has exactly one
kind of conflict; a file API needs `409` and `412` to mean different things, so
it carries its own `FileError` with its own mapping. Worth stating explicitly,
because the temptation to reuse the enum is exactly how the two meanings get
merged.

## The path check

One function, in `crates/shepherd-http/src/files/resolve.rs`, called by every
route and by nothing else:

```rust
fn resolve(roots: &Roots, root: &str, path: &str, intent: Intent) -> Result<PathBuf, FileError>
```

In order:

1. Look `root` up in the enumerated set. Unknown → `404`. (Re-enumerated per
   request, so an unplugged drive fails here rather than deeper.)
2. Reject, on the string, before any syscall: an empty component, `.`, `..`, a
   leading `/`, a NUL, a component containing `/` after decoding. No
   normalisation, no cleverness — a path containing `..` is refused, not
   rewritten, because a rewrite is a place to be wrong.
3. Join onto the root's canonical path.
4. `canonicalize` the *parent* (the target itself may not exist yet) and assert
   it is still beneath the root's canonical path. This is what catches a
   symlinked directory component.
5. Open the final component with `O_NOFOLLOW` for writes, so an upload cannot
   be redirected through a link swapped in after step 4. For reads, `lstat`
   and refuse a symlink whose target leaves the root.
6. Check the denied-subtree list (below).

A TOCTOU window remains between step 4 and step 5 for directory components —
only `openat2(RESOLVE_BENEATH)` closes it completely. Whether to reach for that
syscall is a judgement call for implementation; either way the window gets a
comment naming it, because the attacker who could use it (an activity at the
kiosk uid) can already read and write every one of these files directly. What
this check defends is the *remote* caller, not the local one.

**Denied subtrees inside the home root**, refused for read and write alike:

| Path | Why |
|---|---|
| `~/.local/share/shepherdd/` | The database, and on a device with no state custodian the admin record and `web-auth.toml` too |
| `$XDG_CACHE_HOME/shepherd/` | The video cache keeps an index; hand-deletion desynchronises it, and the directory is machine-managed by design |
| `~/.ssh/` | Added after review — see "What was built" |

`~/.local/state/shepherdd/` (the logs) is deliberately **not** denied: being
able to download `shepherdd.log` from a device with no shell is one of the more
useful things this feature does, and a rotated log a parent deletes costs
nothing.

The escape suite is the test that matters: `../`, `..%2f`, `%2e%2e%2f`,
absolute, `./../`, a component that is a symlink to `/etc`, a symlink created
*between* the listing and the fetch, a path whose parent is a symlink, a NUL,
an over-long component, and a root-relative `//`.

## Config

```toml
# Remote file manager (issue #195) — the Files tab in the management UI.
# Always available to a signed-in administrator; there is no runtime toggle.
[service.file_manager]
# enabled = true                        # false removes the routes entirely
# max_upload_bytes = 8589934592         # 8 GiB; 0 = no cap
# free_space_floor_bytes = 2147483648   # 2 GiB; refuse an upload that would cross it
# external_media = true                 # offer drives mounted under /media and /run/media
#
# Extra places to browse, beyond the home directory and removable drives.
# [[service.file_manager.extra_roots]]
# label = "Shared videos"
# path = "/srv/media"
```

Validation, in `shepherd-config`, refusing at parse time rather than at first
use: `path` must be absolute and must not be `/`, `/etc`, `/var/lib/shepherdd`,
or a parent of any of them; `label` must be non-empty and unique.

`enabled` is a config-time switch, not the toggle that was rejected — a
household that wants the surface absent gets to say so once, in the file that
already decides everything else about the device. Say the word and it goes.

**Adding these fields is not free on the client side.** `schema.rs` is mirrored
into `shepherd-webui/src/config/model/config.generated.ts` by
`cargo run -p shepherd-wire-codegen --bin rpc-codegen`, and
`npm run check:coverage` fails if a generated field is never referenced under
`src/config/` — "a field the editor cannot set is a field nobody can set". So
this table also needs a section in the config editor's service page, or an
`EXEMPT` entry with a reason. That is a real half-day nobody would have costed.

## Where the code goes

- `crates/shepherd-http/src/files/{mod.rs,resolve.rs,roots.rs}` — routes, the
  check, the root enumeration. Not a new crate and not `shepherd-util`: the
  only consumer is this transport, by construction, since nothing here reaches
  BLE.
- `crates/shepherd-http/src/state.rs` — `AppState` gains a
  `files: Option<Arc<FileService>>`, `None` when `enabled = false`, built by
  `shepherdd` at startup. The service reads its settings through the same
  reloadable handle the media sweep uses, so `reload_config` changes the root
  list without a restart.
- `crates/shepherd-http/src/web_assets.rs` — a `Content-Security-Policy` on the
  SPA's own responses, which the daemon does not send today at all. It must
  include `'wasm-unsafe-eval'` (the config editor's validator will not load
  without it, per `CONTRIBUTING.md`) and `style-src 'unsafe-inline'` (MUI's
  emotion injects style tags). `connect-src` stays permissive, because
  `ConnectionSettings` lets the SPA be pointed at another device's `apiBase`;
  the protection that matters here is `script-src 'self'`.
- `shepherd-webui/src/api/files.ts` — hand-written, like `getDeviceConfig`, not
  generated: these are not RPCs.
- `shepherd-webui/src/pages/FilesPage.tsx` + a `NAV` entry in `App.tsx`.
- New dependency: `tokio-util` with the `io` feature, for `ReaderStream` on the
  download path. The workspace has none today. Uploads need nothing new —
  `axum::body::Body::into_data_stream` is enough.

## What this does not do

- **No `ManagementService` methods, no codegen, no Kotlin, no BLE.** Decision 5.
- **No editing in place.** Decision 4.
- **No previews or thumbnails.** They trade directly against the three
  download headers above; worth a follow-up issue once there is a CSP to lean
  on, with a hard-coded type allowlist and `Content-Security-Policy: sandbox`.
- **No copy, no archive extraction, no multi-file zip download.** Each is a
  progress-and-partial-failure problem of its own.
- **No cross-root move.** Refused explicitly rather than half-implemented.
- **No second kiosk user's home.** `shepherdd` runs as one user and reaches
  exactly that user's files; anything else needs a privileged helper, which
  this feature is defined by not needing.
- **No reload or media refresh.** `refresh_media` and `reload_config` already
  exist on the RPC surface; the Files page calls them after a write if it wants
  them, and the file API stays a file API.

## Review checklist for this document

The things most likely to be wrong, listed so a reviewer can go straight at
them:

1. Root ids for removable drives — hash of the mount point, versus the label,
   versus an index. The hash survives two drives labelled `UNTITLED`; nothing
   survives a drive remounted elsewhere mid-session, and the design says so.
2. Requiring a precondition on `PUT` but not on `DELETE`.
3. `etag` as `size-mtime` rather than a content hash, and whether the
   divergence from `PolicyDocument` will confuse the next reader.
4. Denying `~/.local/share/shepherdd/` while allowing `~/.local/state/shepherdd/`.
5. Server-owned sort order, forced by cursor pagination.
6. Whether `enabled` should exist at all, given "no toggle".

## What was built

Reviewed 2026-09-11; the six questions above were answered, and the API was
built server-side the same day. No UI: there is no Files page yet, and the
`shepherd-webui` change in this work is the config editor section the schema
forced (below).

### What the review changed

1. **Removable-drive ids are the filesystem UUID** (`ext-<uuid>`, from
   `/dev/disk/by-uuid`), not a hash of the mount point — so a drive keeps its
   identity when it is unplugged and mounted somewhere else, which the mount
   point does not. A filesystem with no UUID at all falls back to the device
   node (`ext-dev-...`), because "your drive is not listed" is a worse answer
   than an id that does not survive a replug.
2. **`DELETE` requires a precondition too.** The asymmetry the design argued
   for was rejected: both writes now answer `428` without one. A directory has
   no tag, so `If-Match: *` is the only thing that can match one.
3. `enabled` stays, as a config-time switch.
4. Points 3, 4 and 5 (the `size-mtime` tag, the denied subtrees, the
   server-owned sort order) were accepted as designed.

### What building it changed

- **`Located::follow`** was not in the design and is the bug it would have
  shipped with. `resolve` deliberately does not follow the *final* component —
  that is what keeps a link swapped in after the check from being what the
  check looked at — so a listing of `path=escape` followed the symlink itself
  and served whatever it pointed at. Listings and downloads now resolve the
  final component explicitly and re-check it; uploads and deletes still must
  not, and do not. The route test that caught it is
  `a_symlink_out_of_the_root_is_listed_but_not_usable`.
- **`/api/v1` got a JSON 404 fallback.** An unknown path under the nest
  previously fell through to the SPA and answered `200 text/html`, which an API
  client parses as JSON and fails on somewhere else entirely. It is also what a
  caller now sees for a device with `enabled = false`, where the routes are not
  mounted at all.
- **The config editor had to grow a section.** `schema.rs` is mirrored into
  `config.generated.ts`, and `npm run check:coverage` fails on a generated field
  the editor cannot set. `[service.file_manager]` therefore has a section on
  the Service page — `extra_roots` renders through the existing
  `KeyValueEditor` as a label → path map rather than a new repeated-row editor.
  The design predicted this cost; it was half a day, as guessed.
- **`shepherd_util::home_dir()`** is new: `$HOME`, and `None` if it is missing
  or relative, which is the one case where the routes are configured on and not
  served (with a warning saying so).
- **`space()` was not moved to `shepherd-util`**, as the design said it would
  be. `shepherdd::media::free_space` walks up to an existing ancestor because
  it is asked about a cache directory that may not exist yet; every path
  reaching the file manager's has already been canonicalised. Sharing them
  would have meant one function with two meanings, so there are two, and the
  comment on each says why.

### Verified against a live daemon

Not only unit tests: `shepherd dev headless --config` with `auth_token` set,
then `curl`. Roots enumerated the real home and a configured `/srv`-style extra
root with true `statvfs` numbers; `mkdir` → `201`, upload without a
precondition → `428`, with `If-None-Match: *` → `201`, again → `412`; the
download carried `attachment` + `nosniff` + `application/octet-stream`; `Range:
bytes=5-8` → `206` with the right four bytes; `..` → `400`;
`.local/share/shepherdd` → `403`; delete without a precondition → `428`, a
non-empty directory without `recursive` → `409`; and every route without the
token → `401`.

**A trap worth recording**: this machine has an *installed* shepherdd already
bound to `0.0.0.0:8080`, so a dev session configured on the default port
silently talks to it instead — and it is an older build, which looked exactly
like "my routes are not mounted". Give the dev config its own port.

### `~/.ssh`, after a second look

The first build left it readable, on the reasoning that the whole home is the
root (decision 2) and that a caller who can rewrite the policy can already run
anything as this user. That was reported rather than quietly narrowed, and the
answer came back to deny it — rightly. The two existing denials are about
*damage*: a desynchronised cache index, a corrupted database. A private key is
a different thing, a credential for somewhere **else**, and handing one out is
the only power on this surface that "the caller administers this device" does
not already imply. It is denied for writing too, since an `authorized_keys`
dropped in through an upload is as much of a problem as a key read out.

## Three gaps the UI design found (2026-09-12)

Designing the tree (`2026-09-11 005 remote-file-manager-ui (#195).md`) turned up
three places where this contract made a client guess. All three are now fixed,
and the wire shapes above are updated to match.

1. **`usable: false` became `unusable: <reason>`.** One flag covered an
   escaping symlink, a name that is not valid UTF-8, a special file and one of
   the denied directories — and they differ in what they still allow. An
   escaping link can be *deleted*, which is the entire reason it is listed
   instead of hidden; a lossy name cannot be turned back into the bytes that
   address the file, so nothing can be done to it at all. A client handed one
   `false` for both would have to guess which, and would guess wrong in the
   direction that offers a button that cannot work.
2. **Renaming a folder into its own subtree is a `400`.** `rename(2)` answers
   `EINVAL`, which fell through to `FileError::Internal` and reached the caller
   as a `500` — a fault in the device, for a thing a person does by accident
   with a mouse. `EISDIR`, `ENOTDIR` and `ENOTEMPTY` went the same way and are
   now conflicts, since all three mean the target changed kind between the
   check and the call.
3. **Writability is reported per directory.** `Listing::writable` for the
   directory being listed, and `writable` on directory rows; file rows
   deliberately carry none, because deleting a file is a permission on its
   parent and a field on the file would look like the answer without being it.
   One `access(W_OK)` per directory row, which is a local syscall against a
   listing that has already done a `stat` each.
