# A file picker for the config editor (#186) — investigation

**Date:** 2026-09-21
**Issue:** <https://github.com/aarmea/lunchbox/issues/186>

> Status: **investigated, not built.** This is the survey and the design
> argument. Nothing in `lunchbox-webui` changed for it.

## The prompt

> investigate the config-based file picker for #186

[#186](https://github.com/aarmea/lunchbox/issues/186), *"Management-based
config editor improvements"*, lists three quality-of-life items now that the
editor is built in (#185). The second is this one:

> File picker for things like picking a media library, selecting an ebook
> file, tying a RetroArch activity's ROM

The other two — Steam activity autosuggestion and RetroArch backend
autodetection — need the daemon to enumerate something (installed games,
viable cores) that no API reports yet. The file picker does not: everything it
needs already shipped with the remote file manager (#195). That is the reason
to take this one first, and the reason this note is only about it.

## What the fields actually are

Every path a policy can hold, what it means, and whether a picker can serve it:

| Field | Shape the schema demands | Reachable from the file roots? |
| --- | --- | --- |
| `kind.content` (retroarch) | absolute or `~/`; **a directory is legal** — a few cores load one (`crates/lunchbox-host-linux/src/retroarch.rs:350`) | yes — wherever ROMs are kept |
| `kind.book` (ebook) | absolute or `~/` | yes |
| `kind.library` (media) | a path **or a YouTube playlist URL**; `~` expanded at launch | yes, but the field is not path-only |
| `kind.core_path` (retroarch) | absolute or `~/`, and mutually exclusive with `core` | usually — `~/.config/retroarch/cores` is inside the home, but hidden |
| `kind.cwd` (process) | a directory | yes |
| `entry.icon` | a theme *name* or an absolute path | yes, same "not path-only" caveat as `library` |
| `service.file_manager.extra_roots[].path` | absolute directory | **no** — circular: it defines what is browsable |
| `service.{socket_path,log_dir,data_dir,child_log_dir}` | absolute | **no** — outside the roots by design |
| `service.management_api.tls.{cert,key}` | absolute | **no** — same |

The bottom four are the argument against "put a picker on every path field":
the browsable tree is deliberately the kiosk user's home, removable media and
the configured extras, and the service paths are all somewhere else. A picker
there would be a button that opens on nothing.

Three of the top six already have a device diagnostic whose only remedy is
getting the path right — `RetroarchContentMissing`, `EbookBookMissing`,
`RetroarchCoreMissing` (`crates/lunchboxd/src/diagnostics.rs:239`). Those
diagnostics are the measure of whether this feature worked.

## What already exists

The remote file manager (#195) is the whole back end, unchanged:

- `GET /api/v1/files/roots` → `RootInfo[]`: an opaque `id`, a `label`, a
  `kind` of `home` / `external` / `configured`, and a **canonical absolute
  `path` that is display-only** (`crates/lunchbox-http/src/files/roots.rs:37`).
- `GET /api/v1/files/list?root&path` → a `Listing` of `DirEntryInfo`.

Both are plain `GET`s behind the session gate. A picker needs nothing else —
there is no stat route, and it does not need one.

On the client, `src/files/` already has the tree as reusable parts:
`useFileTree` (intent: expansion, selection, sort — single-select already),
`useDirectories` (one react-query per open folder), `buildRows`, and
`MoveToDialog`, which is *the precedent*: the same tree, in a dialog, reduced
to what can be an answer, with its own expansion state so it does not disturb
the tree behind it. `buildRows` already takes `foldersOnly` for exactly that.
A file picker is `MoveToDialog` with the restriction relaxed and the result
handed back instead of acted on.

## The constraint that decides the design

`src/config/` may not import `src/api/`, axios, or react-query. That is not a
convention; `scripts/check-boundary.mjs` fails CI on it, because `src/config/`
also builds into the standalone static bundle, which has no daemon anywhere
near it. `src/files/` imports all three.

So the picker cannot be dropped into `KindEditor`. It follows the shape
`ConfigSource` already established, and `DeviceConfigSource` already documents
in its own header ("the daemon-coupled source lives out here and imports the
*interface* inwards, which is the direction that stays honest in both
bundles"):

1. An interface in `src/config/` — say `FilePicker`, with one method along the
   lines of `pick(request: { kind: "file" | "directory" | "either"; start?:
   string }): Promise<string | null>`, resolving to the string to write into
   the config, or null if cancelled.
2. An implementation outside it — `src/sources/DeviceFilePicker.tsx`, which
   owns the dialog built from `src/files/`.
3. `ConfigApp` takes it as an optional prop, beside `source`, and `App.tsx`
   passes one; `standalone.tsx` does not.
4. It reaches `KindEditor` and `EntryDetail` through a small context, because
   the chain is `ConfigApp → EntriesPage → EntryDetail → KindEditor` and
   prop-drilling a capability through three layers is what contexts are for.
   Absent picker → the field stays exactly the text field it is today. That
   keeps every existing config test rendering `<ConfigApp />` unchanged.

**Standalone must not get a local picker.** The File System Access API is
right there (`FileConfigSource` uses it), and it would pick a path on the
*browsing* computer — a path that means nothing on the device the config is
for. A wrong path that looks authoritative is worse than a text field.

## Turning a pick into a config path

The API speaks `(root id, path relative to root)` and never accepts an
absolute path back. The config wants an absolute path or a `~/` one. The rule
is one line, and it keys off the root *id*, not a string comparison of paths:

- root `home` → `~/<rel>`
- anything else → `<root.path>/<rel>`

`~/` is correct for the home root and not merely equivalent: `expand_tilde`
(`crates/lunchbox-host-linux/src/adapter.rs:107`) resolves it against
`dirs::home_dir()`, and the home root is `lunchbox_util::home_dir()` — the same
`$HOME`, both read in the process that will do the launching
(`crates/lunchboxd/src/main.rs:1878`). Writing `~/` also survives the one way
they can differ: the root's `path` is `canonicalize`d, so a home reached
through a symlink would be written as its target.

For an external root the absolute path is right but the *mount point is not
stable* — `/media/kiosk/ROMS` is wherever udisks put it this boot. Worth a
word in the helper text; the existing diagnostics already catch it after the
fact.

## Edge cases the picker has to get right

- **Names that are not UTF-8.** `DirEntryInfo` reports these with
  `unusable: "name_not_utf8"`, a lossy `name` that addresses nothing, and an
  opaque `handle`. TOML is UTF-8, so such a file *cannot be named by a config
  at all*. The row must be visibly unselectable rather than silently writing a
  path that resolves to nothing. `src/files/permissions.ts` already centralises
  this kind of judgement.
- **Hidden files.** `~/.config/lunchbox/movies.toml` is the canonical media
  library and `~/.config/retroarch/cores` is where `core_path` points. Both are
  hidden, so the picker needs the "show hidden" toggle the Files page has —
  and for `core_path` it should probably default to on.
- **Denied directories.** `.local/share/lunchboxd`, `.cache/lunchbox` and
  `.ssh` are refused by the file service (`files/mod.rs::denied_dirs`). Nothing
  a policy should point at lives there, so this costs the picker nothing.
- **The file manager can be off.** `service.file_manager.enabled = false`
  means the routes are *not mounted*, not that they 403. The picker's roots
  query fails; the button should be absent or disabled with a reason, and the
  text field must keep working. This is also the honest answer for a
  file-manager-less device: the field is still editable, as it is today.
- **`library` and `icon` are not path-only.** A YouTube playlist URL and a
  theme icon name are both legitimate values. The picker is an adornment beside
  the field ("Browse…"), never a replacement for typing.
- **`content` may be a directory**, so its picker asks for "either", while
  `book` and `library` want a file and `cwd` wants a directory.

## What a first cut should be

Smallest thing that closes the issue's own examples: `content`, `book`,
`library`, plus `cwd` and `core_path` because they are the same component with
a different mode. `icon` is the odd one out — a path is the rarer of its two
meanings — and can follow.

The tests that do not need a device: the root+rel → config-path mapping
(pure, and the place a `~/` bug would hide), the unselectable-row rule, and a
render test that `<ConfigApp />` with no picker prop is byte-for-byte the
editor that exists today. The one that does need a device is the whole point
and belongs in the headless dev session: pick a ROM, save, and watch
`RetroarchContentMissing` clear.

## Not in scope here

The issue's other two items. Both need a new read-only enumeration from the
daemon — installed Steam games, and cores that can load a given ROM — and the
second of them wants the ROM the picker just chose, so it is naturally the
work after this one.

## Uploading from inside the editor

Asked as a follow-up: would this also let a parent put the file *on* the
device while configuring, rather than uploading on the Files tab first and
coming back? Yes, and for almost nothing, because the plumbing is already
under the editor.

`App.tsx` renders `<UploadsProvider><AppShell /><TransferTray /></UploadsProvider>`,
and the config editor is the takeover `AppShell` returns when the page is
`config`. So the editor is *already inside the upload queue*: a transfer
started from a picker is the same queue as the Files tab's, it survives
leaving the editor, and the tray already draws over the editor — it is fixed
at `theme.zIndex.snackbar` (1400), above a dialog's `modal` (1300).

`uploads.start({ rootId, dir, files, root, limits })` is self-contained. It
needs a root id, a directory, the `RootInfo` and the `FileLimits` — all of
which a picker holds already, from the same `useFileRoots()` it needs for the
tree. Nothing about it is coupled to `FilesPage`. On completion it invalidates
that directory's listing (`useUploads.tsx:194`), so the uploaded file appears
in the open picker on its own.

Two things the picker has to add, neither of which it needs for reading:

- **Write permission.** Browsing is read-only; uploading is not. An "Upload
  here…" affordance has to be off for a non-writable root or folder —
  `canWriteInto` already answers this — rather than offering a button that
  will 403.
- **Refusals.** `start` returns the files it would not even attempt: over
  `max_upload_bytes`, or under the free-space floor. Those strings are written
  for a person and need somewhere to go.

**The one real decision is the wait.** A 2 GB ROM takes minutes on the wifi
chip these devices came with. The upload lands in `.{name}.{token}.part` and
is published by an atomic rename at the end, and the listing deliberately hides
`.*.part` (`handlers/files.rs:169`) — so mid-flight the final name does not
exist, and letting someone "use" it immediately writes a path to a file that is
not there yet. That is not fatal: the config saves fine, and
`RetroarchContentMissing` clears by itself when the transfer finishes. But it
means a diagnostic fires on a correct configuration for the length of a
transfer, which is exactly the noise the diagnostics exist to avoid.

So: queue the upload from the picker, keep the dialog usable, and enable "Use
this file" when the transfer completes and the row appears. The editor does not
block on it — the document is unsaved and local until someone presses Save, and
the config route and the file routes share nothing but the session, so an
in-flight upload and a config `PUT` do not interact.
