# Enable the config editor in Web management (issue #185) — scope

> Status: **scoped, not built.** This is the survey and the argument. The
> judgement calls it turns on are listed under "Decisions"; everything above
> them is the reasoning that produced the options.

## Prompt

> scope out #185

[Issue #185](https://git.armeafamily.com/albert/shepherd-launcher/issues/185),
*"Enable config editor in Web management"*:

> This is only secure once #156 is in

No comments, no labels. #156 landed in `main` on 2026-09-07 as PR #183.

This is the long-deferred **phase 4** of
[`2026-08-19 002 config-editor-webapp-scope.md`](2026-08-19%20002%20config-editor-webapp-scope.md),
whose phase list reads:

> **Phase 4 — in the management UI.** Gated on privilege separation in
> `shepherd-http` plus new `get_config` / `set_config` RPCs (with
> `reload_config()` already there to apply the result). Then: enable the nav
> item in the embedded target, add `DeviceConfigSource`, and add a
> read-the-current-config-and-diff flow. **The editor itself needs no changes.**

Three of those four clauses have moved since. What follows is what is actually
there now.

## What exists today

### The editor is complete and unrouted

`shepherd-webui/src/config/` is the whole editor: a `toml_edit` document model
in wasm (`crates/shepherd-config-wasm`), a four-op patch vocabulary, undo/redo
with coalescing, three form pages plus a CodeMirror raw pane, and validation by
`shepherd_config::parse_config` compiled to wasm32 — the real parser, not a
reimplementation. It ships today as a **standalone static bundle**
(`dist-standalone/`, `SHEPHERD_UI_TARGET=standalone`) that talks to no daemon.

It is deliberately not routed from `src/App.tsx`. The comment at the top of
that file records why, and it is the closest thing to a spec for this issue:

> Its only ConfigSource is FileConfigSource, which edits a file on whatever
> computer is doing the browsing. In this device's own management UI a "Config"
> tab reads as "edit this device's configuration", which it would not be.

Not routing it also keeps the editor's chunks and its wasm out of `dist/`, and
so out of the daemon binary `rust-embed` builds from it.

### `ConfigSource` is the seam, and it is already the right shape

`src/config/sources/ConfigSource.ts` — `open`/`save`/`saveAs`/`canSaveInPlace`
over a `{ text, name, handle }` document. `ConfigDocProvider` takes a source
**per call** (`openFrom(source)`, `save(source)`) rather than holding one, so
the provider needs no change at all.

`ConfigApp.tsx` is the part that does: it constructs `new FileConfigSource()` at
module scope and names it in five places (Open, Save, Save as…, the
`canSaveInPlace` label, the "this browser cannot write files in place" alert).
So "the editor itself needs no changes" is **no longer true** — `ConfigApp`
needs a source injected. It is a small, contained change, but it is a change.

### The import boundary is enforced

`scripts/check-boundary.mjs`, run by CI, fails the build if anything under
`src/config/` imports `src/api/`, `axios`, or `@tanstack/react-query`. A
`DeviceConfigSource` therefore **cannot live in `src/config/sources/`**. It goes
in the app tree (`src/sources/DeviceConfigSource.ts`) and imports the
`ConfigSource` *type* from `src/config/`, which is the allowed direction.

### There is no `get_config` / `set_config`, and `reload_config` is broken on a
custodial device

The management API is one JSON-RPC endpoint over `ManagementService`. It has
`reload_config` and no way to read or write the config text.

Worse: **`reload_config` reloads the wrong file on any device with a state
custodian.** `DefaultManagementService::reload_config`
(`crates/shepherd-management/src/service.rs:1273`) calls
`load_config(&self.config_path)`, and `config_path` is `args.config` — the home
path — while shepherdd's own reload path
(`crates/shepherdd/src/main.rs:2216`, `handle_config_reload`) reads
`ProtectedFile::Config` through `policy_files` and only falls back to the local
path when there is no custodian. Since #157, the home path on a custodial device
holds the **signpost**: a valid policy with zero entries. So an administrator
pressing "reload" over HTTP or BLE on an installed device replaces the running
policy with an empty one, and the launcher goes blank until something writes the
real file again.

This is a pre-existing bug, not one this issue introduces — but it sits directly
in this issue's blast radius, and the fix (give the service the
`Option<Arc<dyn ProtectedFiles>>` shepherdd already has at
`main.rs:1068`) is the same plumbing a config read/write needs.

### Where the config actually lives, and who may write it

On an installed device: `/var/lib/shepherdd/state/<user>/config.toml`, owned by
the `shepherd-state` uid at mode `0700`, reachable only through the custodian's
socket, which admits exactly one cgroup — the kiosk's logind session scope.
shepherdd is inside it; every activity is not.

The protocol already carries the write: `StateRequest::WriteFile { file:
ProtectedFile::Config, contents }`, served by `LocalProtectedFiles::write`,
which goes **through a temp file and a rename**, so a partial write cannot leave
a corrupt policy. The custodian watches its own directory with `notify` and
pushes a change to shepherdd's `ConfigWatch`, which reloads within a second.

Two consequences worth stating plainly:

- **A write needs no new protocol and no new privilege.** `ProtectedFile::Config`
  is writable over the existing socket today. Its doc comment says "Read by the
  daemon; written out of band by an operator" — that sentence, not a permission
  check, is the only thing that says the daemon doesn't write it.
- **A write reloads itself.** The custodian's watcher (or, without a custodian,
  shepherdd's own `notify` watcher on the config directory) fires on the rename.
  Nothing has to call `reload_config` — which is fortunate, given the bug above.

On a device without the custodian, the config is a plain file in the kiosk
user's home that shepherdd can write directly.

### Auth, after #156

`crates/shepherd-http/src/auth.rs` now distinguishes three credentials and
records which one answered in an `Identity` in the request extensions:

| | |
|---|---|
| `Identity::Session` | a browser signed in by password or by companion approval — `HttpOnly`, `SameSite=Strict`, `Secure` under TLS, plus an `Origin`-vs-`Host` CSRF check on every non-GET |
| `Identity::Machine` | the static `[service.management_api].auth_token`, or the token the BLE claim minted. Authenticates a request; cannot open a session |
| `Identity::Open` | only reachable by an embedding built with no credential store. Never a device |

Open mode is gone from any device. The listener is TLS wherever it binds off
loopback, enforced by config validation.

What #156 did **not** add is per-method authorisation: `handlers/rpc.rs`
dispatches without consulting `Identity`, and every method behind
`require_auth` is equally reachable by all three credentials.

The 2026-08-19 scope gated this issue on exactly that:

> A `set_config` RPC is an RCE primitive behind a blanket auth layer. Phase 4 is
> explicitly gated on privilege separation. Do not ship `set_config` before it.

That argument has weakened, and it is worth being precise about how. #156 landed
`set_web_password`, whose own doc comment settles the same question in the
opposite direction:

> No old password required: the caller has already proved they are the
> administrator by reaching this trait at all — over a bonded BLE link, or with
> a live session.

So the surface behind the gate already includes "take over the device's
management credential permanently". What `set_config` adds on top is **arbitrary
command execution at the kiosk uid** — `RawEntryKind::Process { command, args,
env, cwd }` runs whatever it is given — which is a real escalation over
"administer the device", but at a uid every activity already runs as, on a
daemon that is not root. Whether that gap is worth a distinct control is
decision 1 below.

### What the RPC transport can and cannot carry

`config.example.toml` is **51 KB**. A real device config is the same order.

- **HTTP** is fine: no `DefaultBodyLimit` is set, so axum's 2 MB default applies.
- **BLE is not.** `MAX_FRAME_BYTES` is 16 KiB
  (`crates/shepherd-ble/src/protocol.rs:45`), and `dispatch_json` covers **every
  `async` method on the trait** — so adding `get_config`/`set_config` as trait
  methods automatically exposes them over BLE, where they would fail at the
  framing layer in both directions.

  Two ways out, and the first has precedent: **put the config endpoints on
  dedicated HTTP routes rather than on the trait**, exactly as #156 did with
  the login exchange ("The login itself is not here… a transport that has
  already authenticated its peer has no use for it"). Adding a route to the
  `guarded` router in `handlers/mod.rs` is two lines and is auth-gated by
  construction. The alternative is a skip mechanism in
  `shepherd-management-macros`; note that the macro already skips **non-async**
  trait items, and `ProtectedFiles` is a synchronous interface anyway.

  Dedicated routes also buy `ETag`/`If-Match` for free (see decision 3) and
  avoid JSON-escaping 51 KB of TOML through the RPC envelope.

### The wasm has to be served with the right MIME type

`crates/shepherd-http/src/web_assets.rs::mime_type` has no `wasm` arm, so a
`.wasm` asset is served as `application/octet-stream`. wasm-bindgen's glue
handles it — `instantiateStreaming` throws, and it falls back to
`arrayBuffer()` + `instantiate` — but it logs a console warning saying the
server is misconfigured, and the fallback is slower on a ~1 MB module. One line:
`"wasm" => "application/wasm"`.

### The bundle cost, measured today

The 2026-08-19 figure (1.7 MB → 2.8 MB) predates two phases of editor work, so
it was re-measured on this branch by routing `ConfigApp` behind `React.lazy`,
running `npm run build`, and reverting:

| | bytes | |
|---|---|---|
| `dist/` today | 1 635 281 | |
| `dist/` with the editor routed | 3 136 893 | **+1.50 MB (+92%)** |
| of which the wasm module | 956 494 | 323 kB gzipped |
| release `shepherdd` today | 27 666 088 | so **≈ +5.4%** on the binary |

The wasm and the editor's JS are emitted as **async chunks**, so a browser that
never opens the Config tab never downloads them. The cost is entirely in the
daemon binary and the `.deb`, not in first paint. `rust-embed` here stores
uncompressed and the server sets no `Content-Encoding`, so the wire cost of a
first visit to the tab is ~1.5 MB — once, behind
`Cache-Control: immutable`.

Nothing else in the build needs to change: `build_webui` in
`scripts/lib/build.sh` already calls `build_config_wasm` before `npm run build`,
so every path that builds the embedded UI already requires `wasm-pack`.
CI's `config-editor` job asserts `dist/` stays empty when building the
standalone bundle; that check is about the *standalone* build writing to the
wrong directory and stays correct.

## What has to be built

Ordered as it would be done, with the pieces that can land independently first.

1. **Fix `reload_config` on custodial devices.** Give `DefaultManagementService`
   the `Option<Arc<dyn ProtectedFiles>>` shepherdd already holds, and read the
   policy the same way `handle_config_reload` does. Regression test: a service
   built with a custodian that holds a two-entry policy and a local path holding
   a zero-entry signpost reloads two entries.

2. **`GET /api/v1/config` and `PUT /api/v1/config`** in `shepherd-http`
   (decision 2 may move these onto the trait instead). `text/plain` bodies,
   `ETag` on the read, `If-Match` required on the write.
   - The write **validates with `shepherd_config::parse_config` before it
     writes** — the client-side wasm validator is a UI affordance, not a
     control, and a 422 with the validation error is what a wrong body deserves.
   - The write goes through `ProtectedFiles::write`, which is atomic.
   - It appends an audit event. `AuditEventType` has `ConfigReloaded { success }`
     and nothing for "an administrator replaced the policy"; add one, carrying
     the entry count and enough to identify the caller.
   - Blocking I/O (`RemoteFiles` is synchronous) belongs in `spawn_blocking`.

3. **`DeviceConfigSource`** in `src/sources/`, implementing `ConfigSource`
   against those two routes. `canSaveInPlace` is always true; `open()` fetches
   and stashes the `ETag` in `handle`; `save()` sends `If-Match` and surfaces a
   412 as "someone else changed this file" rather than as a generic failure;
   `saveAs()` has no meaning here and should be either a download or absent.

4. **Inject the source into `ConfigApp`.** A prop for the source, plus whatever
   the toolbar should say when the target is a device rather than a file —
   "Save" instead of "Download", no "this browser cannot write files in place"
   alert, and a decision about whether "Open" and "New" still make sense (see
   decision 4).

5. **Route it** in `src/App.tsx`: a `Config` nav item behind
   `React.lazy(() => import("./config/ConfigApp"))`, and delete the comment that
   says why it isn't there.

6. **Docs.** `crates/shepherd-stated/README.md` says the policy is "written out
   of band by an operator" and `ProtectedFile::Config`'s doc comment says the
   same; both become wrong. `docs/INSTALL.md` lists two ways to change a policy
   and would list three. The 2026-08-19 scope's phase list gets its phase 4
   struck.

## Risks and things that will bite

| | |
|---|---|
| **Two chromes.** `ConfigApp` renders its own full-height `AppBar` + `Tabs` and assumes it owns the viewport. Dropped into `App.tsx`'s content area it nests inside the management UI's drawer/bottom-nav. Either it becomes a full-screen takeover route, or its shell has to collapse into a page. | decision 4 |
| **The editor is desktop-shaped; the management UI is phone-shaped.** dnd-kit drag-and-drop, a week grid, a CodeMirror pane. The management UI's primary form factor is a parent's phone. | decision 4 |
| **Writing a config can lock you out.** `[service.management_api]` is *in* the file being edited. A save that binds the API to loopback, or turns it off, ends the session that made it — and on a hardened device there is no SSH to fix it. Worth at least a confirmation, and arguably a validation rule the write rejects. |  |
| **A save is a policy reload, immediately, mid-session.** The watcher fires on the rename. A child playing something whose entry just changed sees the new limits within a second. That is the existing behaviour of `sudoedit` too, so it is not new — but it is new that a parent can trigger it from a phone without meaning to. |  |
| **Concurrent editors.** `sudoedit`, `shepherd-admin policy`, and now a browser. Without `If-Match` the last save wins and silently discards the other's work. |  |
| **A device whose config does not parse.** shepherdd keeps the running policy and warns; the editor would open a document its own wasm reports as broken, which is exactly the case it is most useful for. Make sure `GET` returns the bytes regardless of validity. |  |
| **`config_version` skew** is not a risk in the embedded build — the wasm and the daemon come out of the same tree and the same `cargo build`. It stays a real risk for the standalone editor, unchanged. |  |

## Verification

Unit and integration coverage is straightforward (`crates/shepherd-http/tests/api.rs`
for the routes, a `dispatch.rs` test for the reload fix, vitest for
`DeviceConfigSource` against a mocked client). What needs the real stack:

- The headless dev session (`./scripts/shepherd dev headless` → `dev shot` →
  `dev stop`, per the `headless-dev` skill) for the tab itself — sign in, open
  the editor, change a limit, save, and watch the launcher grid change.
- A device with the custodian, for the write-through-the-socket path and the
  auto-reload. This is the half that cannot be faked: the dev stack runs without
  `shepherd-stated` unless it is installed.
- `sudoedit` under a browser holding a stale document, for the 412.

## Decisions

To be answered before implementation.

Answered 2026-09-07.

### 1. The write gate: blanket auth is enough

Ship `PUT /api/v1/config` behind the existing `require_auth`, with no
per-identity rule and no step-up. The precedent is `set_web_password`, which
#156 shipped with "no old password required: the caller has already proved they
are the administrator by reaching this trait at all". A surface that already
lets a caller take the device's management credential permanently is not made
safe by refusing them a config write.

**The 2026-08-19 phase list's "gated on privilege separation" clause is struck,
not satisfied.** That should be written into
`2026-08-19 002 config-editor-webapp-scope.md` and into `src/App.tsx`'s replaced
comment, so the next reader does not go looking for a control that was decided
against rather than forgotten.

What the write still owes, none of it identity-based: server-side
`parse_config` before the bytes land, an audit event, and `If-Match`.

### 2. Transport: dedicated HTTP routes

`GET /api/v1/config` and `PUT /api/v1/config` in `handlers/mod.rs`'s `guarded`
router, `text/plain` bodies, `ETag` on the read and `If-Match` required on the
write. Not `ManagementService` methods — so `dispatch_json` never sees them and
BLE never has to carry 51 KB through a 16 KiB frame cap. Same shape #156 used
for the login exchange, and the same reason: a transport that has no use for a
method should not be made to grow one.

The handler needs the policy file, which means `AppState` (or a small trait it
holds) gains the `Option<Arc<dyn ProtectedFiles>>` plus the local path —
the same pair decision 4 gives the management service. `ProtectedFiles` is
synchronous, so the call belongs in `spawn_blocking`.

Consequence to record: **the companion cannot edit a config**, now or without
further work. That is the status quo and nothing regresses, but it is now a
decision rather than an accident.

### 3. UI shape: full-screen takeover

A `Config` nav item opens `ConfigApp` over the whole viewport, with its own
AppBar and tabs as they are, plus a Back control returning to the management UI.
`ConfigApp` keeps its shell; what it gains is the source prop and the close
affordance.

This is also the answer to the form-factor worry: the editor is not reshaped for
a phone, it simply gets the whole phone. The drag-and-drop grid stays cramped
there and that is accepted for now rather than solved.

### 4. `reload_config` on custodial devices: fixed here

`DefaultManagementService` gains the `Option<Arc<dyn ProtectedFiles>>` shepherdd
already holds at `main.rs:1068`, and `reload_config` reads the policy the way
`handle_config_reload` does — custodian first, local path only when there is no
custodian holding one. Regression test: a service whose custodian holds a
two-entry policy and whose local path holds a zero-entry signpost reloads two
entries, not zero.

It ships in this work because it is the same plumbing, and because leaving a
button that blanks the launcher on the screen this issue adds is not a thing to
do deliberately.

## Build order, after the decisions

1. `reload_config` fix + the `ProtectedFiles` handle on the management service
   (decision 4). Independently mergeable.
2. `"wasm" => "application/wasm"` in `web_assets.rs::mime_type`. One line,
   independently mergeable.
3. `GET`/`PUT /api/v1/config` with ETag/If-Match, server-side validation, and a
   new `AuditEventType` for a policy written through the API.
4. `src/sources/DeviceConfigSource.ts` — outside `src/config/`, because
   `check-boundary.mjs` forbids an `src/api/` import from inside it.
5. `ConfigApp` takes its source as a prop, and a close control.
6. Route it in `src/App.tsx` as a full-screen page; replace the comment that
   explains its absence with one recording what was decided in 1 and 3 above.
7. Docs: `crates/shepherd-stated/README.md` and `ProtectedFile::Config`'s doc
   comment both say the policy is "written out of band by an operator";
   `docs/INSTALL.md` lists two ways to change a policy and now has three; the
   2026-08-19 scope's phase 4 is struck.

Accepted cost, measured: `dist/` 1.64 MB → 3.14 MB, so about +5.4% on a 27.7 MB
release `shepherdd`. The editor's JS and its 956 kB wasm are async chunks, so a
browser that never opens the tab never fetches them.

---

## Implementation record (2026-09-07)

Built as scoped, with two findings the survey did not have.

### What shipped

| | |
|---|---|
| `crates/shepherd-management` | `PolicyDocument` (text + a SHA-256-derived version tag); `read_policy`/`write_policy` on the trait — **non-async**, so `#[management_rpc]` skips them and BLE never sees a 51 KB config against its 16 KiB frame cap; `policy_files` on `DefaultManagementService`; one `policy_text()` helper that `reload_config` now goes through too |
| `crates/shepherd-http` | `handlers/config.rs` — `GET`/`PUT /api/v1/config`, `text/plain`, `ETag`, `If-Match` **required** (428 without, 412 on mismatch), server-side `parse_config` before the write (422), inside the existing `require_auth`; `"wasm" => "application/wasm"` in `web_assets.rs` |
| `crates/shepherd-store` | `AuditEventType::PolicyWritten { entry_count }` — distinct from the engine's `PolicyLoaded`, which the reload a second later also records |
| `crates/shepherdd` | passes `policy_files` through; **fixed the config watcher** (below) |
| `shepherd-webui` | `getDeviceConfig`/`putDeviceConfig`; `src/sources/DeviceConfigSource.ts`; `ConfigApp` takes `source`/`autoOpen`/`onClose`; `DangerZone` around the management API section; the Config route in `App.tsx` |

`ConfigDocProvider.save` now returns whether the document reached the source,
so the editor can say "Saved" for a device without saying it after a failure.

### Finding 1 — the watcher never fired on a relative `--config`

The design rests on "the write does not reload; the rename does". On the dev
stack it did not: a plain `touch config.example.toml` produced no reload
either, so this was pre-existing and total.

`watch_policy` compared **whole paths** — the one `--config` was spelled with
against the one `notify` reports. Every dev entry point passes
`-c ./config.example.toml`, which never matches an absolute event path, so
auto-reload was off for the entire development stack while the log line said
`Watching config file for changes`. On a device the path is absolute, so it
worked, which is why nobody had seen it.

Now matched on **file name** — the watch is one non-recursive directory, so a
name is unique within what it can see — with `Path::parent`'s `Some("")` for a
bare filename mapped to `.`. The predicate is a free function,
`policy_event_matches`, with four unit tests that need no filesystem, no
working directory and no timing window.

### Finding 2 — "the editor needs no changes" was already known to be wrong, but so was "add a page"

`ConfigApp` named `FileConfigSource` in five places, as the scope said. What
the scope did not anticipate is that a device wants a **different toolbar**,
not the same one pointed elsewhere: "Open" becomes "Reload" (a device has one
config and it is already open; the useful verb is "show me what is actually
there now"), and "New" is not offered at all, because beside a live config it
reads as an offer to blank it.

### The Danger Zone

Asked for during implementation, and it earns its border: `[service.management_api]`
is *in* the file being edited, so a save that disables the API, moves it out of
reach, or drops TLS on a non-loopback bind ends the session that saved it —
and `harden apply` leaves no SSH to go back in with. Every other section on
that page is recoverable by editing it again from the same place.

`src/config/components/DangerZone.tsx`, wrapped around that one `Section`.
Deliberately not around Bluetooth management: losing BLE does not lock anyone
out of HTTP, and a danger zone containing everything warns about nothing.

### Verified on the headless dev stack

Not just built — driven, with `dev headless` + Firefox over Marionette:

- `GET /api/v1/config` returned the whole 51 KB `config.example.toml` with an
  `ETag`, `text/plain`, `no-store`.
- `PUT` with that tag changed an activity's label; the launcher grid showed the
  new label **within a second**, with no reload call — the journal shows
  `Policy replaced through the management API` followed by `Config reloaded`.
- 412 on a stale tag, 428 with no `If-Match`, 422 on TOML that does not parse
  (with the parser's own line/column), 401 with no credential. Nothing was
  written in any of the three failure cases.
- The wasm module serves as `application/wasm`.
- **Comment preservation, end to end**: an edit made in the browser's form and
  saved to the device changed exactly one line of a 51 KB file, with all 666
  comment lines intact (`git diff --stat`: `1 insertion(+), 1 deletion(-)`).
- **The concurrency guard, end to end**: with an editor open, a `sed` against
  the file (standing in for `sudoedit`) then a save from the browser produced
  *"The config on the device changed while this was open — someone edited it
  over SSH, or from another browser. Reload to see what is there now; this copy
  has not been saved."* The SSH edit survived.

Bundle cost, re-measured on the shipped build: `dist/` 1 635 281 → 3 140 996
bytes.

### Not done

- **The companion cannot edit a config.** That is the status quo and nothing
  regresses, but it is now a decision (the endpoints are off the trait) rather
  than an accident.
- **No diff-before-save view.** Phase 3 of the 2026-08-19 list, still open, and
  more useful now than it was: `If-Match` tells you *that* someone else wrote
  the file, and a diff would tell you what they wrote.
- **The editor on a phone** is the same desktop-shaped editor, now with the
  whole viewport. Accepted, not solved.
