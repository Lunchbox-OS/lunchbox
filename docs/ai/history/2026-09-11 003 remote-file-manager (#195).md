# Remote file manager (issue #195) — scope

> Status: **scoped, not built.** This is the survey and the argument, not an
> implementation. The five questions it turns on were answered on 2026-09-11
> and are recorded under "Decisions" at the end; the body above them is the
> reasoning that produced the options, kept as written so a later reader can
> see what was rejected and why. The wire contract that follows from them is
> `2026-09-11 004 remote-file-manager-api (#195).md`.

## Prompt

> scope out #195

[Issue #195](https://git.armeafamily.com/albert/shepherd-launcher/issues/195),
*"Remote file manager"*, opened 2026-09-10, no labels, no comments:

> While setting up activities, it may be useful to be able to remotely manage
> files without having to use `scp`, `rsync`, or an SFTP-capable file manager
> -- especially since these cannot access hardened user accounts.
>
> This could be as simple as having a toggle in the management UI that
> temporarily enables something like
> [copyparty](https://github.com/9001/copyparty) pointed at that kiosk user's
> home directory.

## The problem is real, and the issue understates it

"Cannot access hardened user accounts" is exact. `scripts/lib/harden.sh` writes
`/etc/ssh/sshd_config.d/shepherd-<user>.conf` containing `DenyUsers <user>`,
and `chmod 0700`s the home directory. So on a hardened device:

- `scp kiosk@device:` — refused by sshd. There is no `su` into the account
  either; `docs/INSTALL.md:638` says so in as many words.
- `scp admin@device:` then `sudo mv` — works, and produces **root-owned files
  in the kiosk's home**. The kiosk user can read them only because the modes
  happen to be permissive; anything shepherd writes back beside them (Okular's
  reading position, a poster cache) is a different owner. This is the footgun
  the current workflow hands people.
- An SFTP file manager — same door as `scp`, same refusal.

And the set of files a parent legitimately needs to put there is not small:

| What | Where | Which diagnostic says it is missing |
| --- | --- | --- |
| A book | `~/Books/*.epub`, `~/Books/covers/*.png` | `EbookBookMissing` |
| A ROM or disc image | wherever the RetroArch entry points | `RetroarchContentMissing` |
| A local video | `file:///home/<user>/Videos/...` per `movies-library.example.toml` | — (`MediaLibraryUnreadable` if the library file itself is bad) |
| A media library file | `~/.config/shepherd/movies.toml` | `MediaLibraryUnreadable` |
| An entry's icon | anywhere the entry's `icon =` points | — |

Three of those already have a diagnostic whose *only* remedy is getting a file
onto the device. The config editor (#185) can now write the policy that names
the file; nothing can put the file there. That gap is the issue.

Note what is *not* in the list: `config.toml`. Since the state custodian
(#157) it lives at `/var/lib/shepherdd/state/<user>/config.toml`, outside the
home, and the web config editor already edits it over `GET`/`PUT
/api/v1/config`. A file manager is for content, not policy.

## What exists today, and what it means for where this can live

**`shepherdd` runs as the kiosk user**, inside its sway session
(`crates/shepherd-firewall-helper/README.md`: "shepherdd runs as the kiosk user
(no caps)"). So the daemon *already* has exactly the credentials needed to
read and write that home directory, and files it creates there are owned by the
right user with the right umask. No privileged helper is required for this
feature. That is the single most important fact in this document.

**The management API is already authenticated properly.** Since #156:
sessions rather than a naked shared secret, `HttpOnly; SameSite=Strict` cookie,
an `Origin`-vs-`Host` check on cookie-authenticated writes
(`crates/shepherd-http/src/auth.rs:248`), per-address lockout with doubling
backoff, and TLS that is on by default on any non-loopback bind
(`[service.management_api.tls] mode = "auto"`). Anything reusing that gate
inherits all of it. Anything standing beside it inherits none of it.

**The SPA and the API share an origin.** The daemon serves `shepherd-webui`'s
`dist/` from a `rust_embed` fallback (`web_assets.rs:30`) on the same listener
as `/api/v1`. That is what makes the cookie work — and, below, what makes
serving arbitrary uploaded files back from that origin dangerous.

**Authority behind the gate is already total.** `PUT /api/v1/config` accepts a
policy containing `kind = { type = "process", command = "..." }`, which is
arbitrary command execution as the kiosk user at the next launch.
`shepherd-webui/src/App.tsx:43` records the decision not to gate the config
editor any further than the session gate, with the reasoning: "A surface that
already hands over the device's credential is not made safe by withholding a
config write." The same sentence applies to a file write, and it is what makes
the "temporarily enables" half of the issue worth re-examining (Q1 below).

**Spawning helper processes is constrained.** `clippy.toml` bans
`Command::new` outright: a helper must be resolved from a compiled-in list of
root-owned directories, because a direct child of `shepherdd` lands in
`shepherdd`'s own cgroup, which the IPC socket accepts as `ClientRole::Admin`
(`crates/shepherd-ipc/src/peer.rs:499`). `yt-dlp` is the standing precedent for
the exception: because it parses whatever a remote host returns, it is
deliberately launched into a transient scope of its own, *like an activity*,
rather than into the daemon's cgroup (`docs/INSTALL.md:519`). Any spawned
file server would have to follow the `yt-dlp` rule, not the sidecar rule.

**The contrary precedent is worth quoting**, because this feature is its
mirror image. `ProtectedFile` is an enum rather than a path
(`crates/shepherd-util/src/paths.rs:250`):

> **An enum rather than a path**, deliberately. […] a request that carried a
> *name* would need validating against an allow-list on every call — a check
> that can be got wrong once and then serves arbitrary files out of a directory
> whose whole point is that nothing else can read it.

A file manager is precisely an API that carries arbitrary names. The design
below cannot dodge that; it can only put the check in one place, test it hard,
and keep the reachable tree away from anything the custodian holds.

## Three shapes

### A. Spawn copyparty, as the issue suggests

`shepherdd` starts `copyparty` when the toggle goes on, kills it when it goes
off.

- **Cost paid once**: a new runtime dependency (copyparty is Python; it would
  become a `Suggests:` of the `.deb` alongside `adb` and `curl`, and a
  `shepherd-admin apps install` target), plus supervision code, plus a second
  listening port.
- **Cost paid forever**: it is a **second front door with its own
  authentication**. Everything #156 built — sessions, lockout, the cookie, TLS
  by default, the `Origin` check, revocation from the sessions list — stops at
  copyparty's edge. Its own accounts file becomes a second credential to
  store, rotate, and factory-reset, and its listener is plaintext HTTP on the
  LAN unless separately configured, on a device whose threat model is a
  motivated child with a laptop on that LAN.
- Plus the cgroup problem above: it must go into a transient scope like
  `yt-dlp`, and it must not be located by `$PATH`.

The issue's "as simple as" is doing a lot of work. This is the option with the
smallest diff and the largest permanent surface.

### B. Spawn copyparty on loopback, reverse-proxy it through the authenticated listener

Fixes the authentication objection: the only way in is through `require_auth`.
Keeps the dependency, the supervision, and the process. Adds a proxy — and
`axum` has no built-in one, so it is hand-rolled streaming in both directions,
including the upload path, which is the part that has to be right.

The catch is that loopback is not a boundary here: **every activity runs at
`shepherdd`'s uid**, so the child's browser activity could reach copyparty's
loopback port directly if its Chromium URL allowlist let it. Chromium's
allowlist is authoritative (`config.example.toml:853`), so the default answer
is "no" — but it becomes one more thing that has to stay true, and a
`[entries.firewall]` `allow = ["localhost"]` would undo it silently.

### C. Serve it natively from `shepherd-http`

New routes under `/api/v1/files`, inside the existing `require_auth` nest, plus
a page in the SPA.

- Inherits sessions, lockout, TLS, the `Origin` check, revocation, the login
  screen, and the `/api/v1` schema conventions. No second credential exists to
  be forgotten.
- No new runtime dependency, no new port, no new process, no cgroup question.
- The endpoints are **HTTP routes, not `ManagementService` methods**, for the
  same reason `read_policy`/`write_policy` are not RPCs
  (`crates/shepherd-management/README.md`): `#[management_rpc]` carries every
  `async` trait method to BLE, and BLE's frame cap is 16 KiB. A file transfer
  on that surface would be a method that exists and cannot work. Only a
  *toggle*, if there is one, belongs on the trait — so the companion can flip
  it — and that is a cheap `bool`.
- The cost is that we write the file manager: listing, upload, download,
  rename, delete, mkdir, and a UI for all of it. This is the real work, and
  most of it is the UI.

**Recommendation: C.** The decisive argument is not effort, it is that A and B
put a file server on a device next to an authentication system that took an
entire issue to get right, and give it none of it.

## What C actually involves

### The path check, in one place

One function: given a caller-supplied relative path, produce an absolute path
or an error. Everything else calls it and nothing else touches user input.

- Reject absolute paths and any component that is `..` *before* touching the
  filesystem — string-level, so a non-existent intermediate cannot be used to
  escape.
- `canonicalize` the parent and confirm it is still under the root. For a
  create, canonicalize the parent, not the target.
- **Symlinks are the sharp edge.** The kiosk user owns the whole tree and can
  create a symlink to `/` at any time — and so can any activity, which is the
  one thing here that an activity can genuinely influence. Refuse to traverse a
  symlink whose target leaves the root; refuse to *follow* one on write
  (`O_NOFOLLOW` on the final component) so an upload cannot be redirected.
  There is a TOCTOU window between `canonicalize` and `open` that only
  `openat2(RESOLVE_BENEATH)` closes properly; whether to reach for that is a
  judgement call, but the window should at least be named in the code.
- Refuse to follow a hardlink count > 1 on overwrite? No — out of proportion.
  Say so in a comment so the next reader knows it was considered.

Tests: a table-driven suite of escape attempts (`../`, `..%2f`, absolute,
`./../`, a symlink to `/etc`, a symlink created *between* list and fetch, a
path whose parent is a symlink), because this is the check the `ProtectedFile`
comment warns about.

### Download is where the XSS is

Serving an uploaded file back from the SPA's own origin is the single most
dangerous line in this feature. A `.html` (or `.svg`) uploaded and then opened
in a tab runs script **on the management origin**, where it can call
`/api/v1/rpc` with the browser's cookie attached. `HttpOnly` does not help;
the script does not need to read the cookie, only to be sent with it.

So, all three, not one:

- `Content-Disposition: attachment` on every file response, always.
- `X-Content-Type-Options: nosniff`, and a fixed `application/octet-stream`
  rather than a guessed type. (A preview feature — thumbnails, "view this
  poster" — trades directly against this and should be deferred, or served
  under `Content-Security-Policy: sandbox` with a hard-coded type allowlist.)
- A `Content-Security-Policy` on the SPA's own responses, which the daemon does
  not send today at all. Worth adding regardless of this feature.

`Range` support matters for downloads of large files; `rust_embed`'s handler
has none, so this is new code either way.

### Upload

`axum`'s `Multipart` needs the `multipart` feature, which the workspace does
not currently enable — or skip multipart entirely and take a raw body on `PUT
/api/v1/files/<path>`, which is simpler, streams naturally, and is what the
config editor already does for TOML. Prefer the latter; the browser side is
`fetch(url, {method: 'PUT', body: file})` and gets upload progress from a
`ReadableStream`.

Must-haves: stream to a temp file in the destination directory and `rename`
into place (so a failed upload never half-replaces a file a child is reading);
a configurable size cap; a free-space floor before accepting, reusing the
`free_space_floor_bytes` idea from `[service.media]` — filling the disk on a
kiosk is a session that will not start.

### After the file lands

The workflow ends at "the activity now works", not "the file is on disk":

- `refresh_media` already exists on the trait for exactly this kind of "I
  changed something, re-read it" moment.
- `reload_config` re-evaluates entries, which clears `EbookBookMissing` and
  `RetroarchContentMissing`.
- The genuinely nice version: the Diagnostics page shows a missing-file
  diagnostic with an **"upload here"** action that opens the file manager at
  that path's directory. `Diagnostic` carries a `DiagnosticSubject::Entry`, and
  the entry knows the path. This is the difference between a file manager and
  a *setup* tool, and it is maybe a day of work on top.

### The UI

A new page in `shepherd-webui` (`src/pages/FilesPage.tsx`, a nav entry in
`App.tsx`'s `NAV`), MUI like everything else: breadcrumb, a table of
name/size/modified, drag-and-drop upload with progress, rename, delete with
confirmation, new folder. Phone-first, because the parent doing this is holding
a phone — the existing pages already use `useMediaQuery` for the drawer/bottom-nav
split. Everything embeds into the daemon binary via `rust_embed`, so this costs
binary size; a plain file table is small next to the config editor's ~1.5 MB.

## Effort

| Piece | Size |
| --- | --- |
| Path resolution + its test suite | 1 day, and the one to get right |
| List / download / delete / mkdir / rename routes | 1 day |
| Upload (streaming, temp+rename, caps, free-space floor) | 1 day |
| Security headers (CSP, `nosniff`, `Content-Disposition`) | half a day |
| Config: roots, size cap, enable flag; `config.example.toml` + validation | half a day |
| SPA page | 2 days |
| Diagnostics → "upload here" wiring | 1 day, optional |
| Toggle, if there is one: trait method + codegen + event + UI | 1 day |
| e2e coverage in `crates/shepherd-e2e` | half a day |
| Docs: `crates/shepherd-http/README.md`, `docs/INSTALL.md`, `CONTRIBUTING.md` | half a day |

Roughly **a week**, or four days for a version with no toggle and no
diagnostics wiring. Option A looks like two days and then never stops costing.

## Decisions to make

**Q1 — Is there a toggle at all?** The issue says "temporarily enables". That
instinct comes from copyparty, which would be a second unauthenticated door;
behind #156's gate the reasoning changes. A caller who can reach these routes
can already `PUT /api/v1/config` with a `type = "process"` entry and run
anything as the kiosk user, so a timed toggle guards a door next to an open
one — and a parent who opens the app to fix a book and finds the file manager
switched off is a parent doing two steps instead of one. Against: the config
editor cannot *exfiltrate*, and a file manager can — it reads the home
directory, which on a device without a state custodian still holds
`~/.local/share/shepherdd`. **Recommendation: no toggle**, matching the
`App.tsx:43` precedent, with an `enabled` key in `config.toml` for a household
that wants the surface absent entirely. If a toggle is wanted anyway, model it
on admin mode (#154): a trait method so the companion can flip it over BLE, an
`Event` so every shell sees the change, an idle expiry, and off at boot.

**Q2 — What is the root?** The whole kiosk home, or a configured list
(`~/Books`, `~/Videos`, `~/.config/shepherd`, …)? A list is tighter and is
one more thing to get wrong when a parent puts a ROM somewhere unlisted and the
manager cannot see it. **Recommendation: the whole home**, dotfiles hidden by
default but reachable (`~/.config/shepherd/movies.toml` is a file a parent
genuinely edits), with a hard refusal list for directories shepherd manages
itself — `$XDG_CACHE_HOME/shepherd/media/` in particular, where the video cache
keeps an index that hand-deletion would desynchronise. Offer a config key to
override the root for a household that keeps content on `/srv`.

**Q3 — Does this reach outside the home?** A device with media on an external
drive mounted at `/media/...` is plausible. Configurable extra roots, each
checked the same way, is a small addition if it is designed in now and awkward
if retrofitted.

**Q4 — Editing text in place?** `~/.config/shepherd/movies.toml` is the one
file a parent is likely to want to *edit* rather than replace, and the config
editor's Monaco-free plumbing is already in the bundle. Suggest: out of scope
for v1; download-edit-reupload works, and `shepherd-media validate` is the
thing that actually catches mistakes.

**Q5 — Does the companion get anything?** Uploading from a phone over BLE is
not viable (16 KiB frames). The companion's honest role is to flip the toggle
if Q1 keeps one, and to tell the parent the device's address so they can browse
to it — which `network_status` (#182) already does.

## Explicitly out of scope

- Any path to a *second* kiosk user's home. `shepherdd` is per-user and runs as
  that user; reaching another one means a privileged helper, and this feature
  does not need one.
- Serving the file manager to anything but an authenticated administrator. In
  particular, not to the launcher or the HUD: a file browser on the device's
  own screen is a file browser the child is sitting in front of.
- Previews and thumbnails — see the XSS argument. Worth a follow-up issue once
  a CSP is in place.

## Decisions

Answered 2026-09-11.

1. **No toggle.** The surface is always available to an authenticated
   administrator, as the config editor is. An `enabled` key in `config.toml`
   remains as a config-time off switch — not a toggle in the UI, and up for
   removal if it reads as one.
2. **Root is the whole kiosk home**, dotfiles hidden by default but reachable,
   with shepherd's own data and cache directories denied outright.
3. **Removable drives are autodetected** at `/media/...` (and `/run/media/...`),
   *and* additional directories are configurable. Roots are therefore a set the
   server enumerates, not a constant — which is the decision that shaped the
   API most.
4. **No in-place text editing.** Out of scope for now; download, edit, upload.
5. **Nothing in the companion.** Which means nothing on `ManagementService`
   either: HTTP routes only, no codegen, no Kotlin, no BLE.
