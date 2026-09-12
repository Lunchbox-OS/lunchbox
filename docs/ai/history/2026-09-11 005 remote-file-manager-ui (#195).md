# Remote file manager (issue #195) — UI component architecture

> Status: **designed, not built.** The server side shipped in
> `2026-09-11 004 remote-file-manager-api (#195).md`; this is the web UI that
> sits on it. Written to be argued with before any component exists.

## Prompt

> write up a component architecture for the UI for this. the UX I'm going for
> is roughly the list view in the macOS Finder, including some columns,
> sorting, and expandable folders, starting from each root. that way there is
> only one root view, and then you expand into it to navigate to the folder you
> want. there should be drag and drop onto each folder to upload, drag and drop
> between folders to move, and context menu/three dots options for rename and
> delete.

## What "Finder list view" buys, precisely

One screen, no navigation state. There is no current directory, no breadcrumb,
no back button, and therefore no history to keep in sync with a URL. A row's
position *is* its path, and the whole device is one tree whose top level is the
roots (`Home`, `KINGSTON`, `Shared videos`). Expanding a root lists its top
level; expanding a folder inserts its children beneath it.

That matters more here than it would elsewhere because the device has **several
roots** — the home directory, every removable drive, and every configured extra
— and the alternative to a tree is a place-picker plus a folder view, which is
two screens and a mode. It also means a parent copying a book from a USB stick
into `~/Books` can see both ends of the move at once, which is exactly the
gesture the drag-to-move is for.

The costs, taken deliberately:

- **Sorting has to be ours, not the server's.** See "Sorting" below.
- **A deep tree can get long.** Nothing paginates a *tree*; each folder
  paginates separately, and the flat row list is the sum of what is expanded.
- **Every gesture needs a non-gesture equivalent**, because a phone has no drag
  and a screen reader has no drop. The ⋮ menu is not a convenience; it is the
  real interface, and drag-and-drop is the accelerator.

## The load-bearing decision: intent in a reducer, facts in the query cache

The tree is not a data structure the UI owns. It is a *projection* of two
things:

| | Lives in | Why |
| --- | --- | --- |
| Which nodes are expanded, what the sort is, what is selected, what is being renamed | A `useReducer` in `useFileTree` | Intent. Survives refetches, is never invalidated by the server. |
| What is *in* each directory | React Query, one query per directory | Facts. Refetched, invalidated per folder after a write, shared between rows. |

Keeping them apart is what makes the awkward cases fall out for free: a folder
refetched after an upload re-renders in place without collapsing; two rows that
name the same directory (they cannot today, but a future "favourites" row
could) share one fetch; and a stale tree is a cache problem rather than a tree
problem.

```ts
// A node's identity. NUL separates because it cannot occur in either half —
// a root id is `[a-z0-9-]` and the API refuses a path component containing it.
type NodeKey = `${string}\0${string}`;          // `${rootId}\0${path}`

type TreeNode =
  | { kind: "root"; key: NodeKey; root: RootInfo }
  | { kind: "entry"; key: NodeKey; rootId: string; path: string; entry: DirEntryInfo };

interface Row {
  node: TreeNode;
  depth: number;          // 0 for roots; drives the name cell's indent
  expandable: boolean;    // a root, or a usable directory
  expanded: boolean;
}

interface TreeState {
  expanded: Set<NodeKey>;
  selected: NodeKey | null;       // single selection in v1; see "Later"
  sort: { column: "name" | "size" | "modified" | "kind"; direction: "asc" | "desc" };
  foldersFirst: boolean;          // default true
  showHidden: boolean;            // default false
  renaming: NodeKey | null;
}
```

`visibleRows` is a `useMemo` walk: for each root in `useFileRoots()` order, emit
its row; if expanded, recurse into that directory's cached entries, sorted and
filtered. A directory that is expanded but not yet loaded emits skeleton rows at
the right depth, so the tree does not jump when the answer arrives.

**Pruning is the subtle part.** Deleting or moving a folder must remove every
`expanded` key beneath it, or a new folder created later with the same name
arrives mysteriously pre-expanded — with a *stale* subtree under it. One
reducer action, `forgetSubtree(key)`, called from the delete and move
mutations, and tested.

## Component tree

```
FilesPage                         src/pages/FilesPage.tsx
├── FilesToolbar                  refresh · new folder · upload · show hidden · sort (mobile only)
├── FileTreeTable                 role="treegrid", the whole tree as flat rows
│   ├── FileTableHead             sortable column headers (desktop only)
│   └── FileRow  × n              memoised; one per visible row
│       ├── NameCell              indent + chevron + icon + name | RenameField
│       ├── SizeCell              bytes, or free/total for a root row
│       ├── KindCell              from the extension; "Folder", "Drive"
│       ├── ModifiedCell          relative date, absolute in a tooltip
│       └── RowActionsButton      ⋮ → RowActionsMenu
│   ├── SkeletonRows              while a directory loads, at its depth
│   ├── DirectoryErrorRow         inline, with Retry; does not collapse the node
│   ├── EmptyFolderRow            "Nothing in here"
│   └── ShowMoreRow               only when a folder exceeded the page bound
├── TransferTray                  bottom sheet: uploads in flight, progress, retry, cancel
└── dialogs
    ├── NewFolderDialog
    ├── DeleteConfirmDialog       names what goes, and says "and everything in it"
    ├── MoveToDialog              the touch/keyboard equivalent of a drag
    ├── ReplaceConfirmDialog      the 412-on-create answer
    └── ConflictDialog            the 412-on-replace answer: reload and redo
```

Hooks and non-visual modules:

```
src/api/files.ts        hand-written client (these are not RPCs, like getDeviceConfig)
src/files/useFileRoots.ts    query: roots + limits
src/files/useDirectory.ts    infinite query: one directory
src/files/useFileTree.ts     the reducer + visibleRows
src/files/useFileActions.ts  mkdir / move / delete / rename, with preconditions and invalidation
src/files/useUploads.ts      the transfer queue, in a context
src/files/dnd.ts             drop eligibility, DataTransfer helpers — pure, and tested
src/files/format.ts          bytes, dates, "kind" from an extension
src/files/types.ts
```

**Why a feature folder** rather than the existing `src/pages` + `src/components`
split: this is ~12 modules with one subject, and scattering them into a
components directory that currently holds three files makes both worse. The
`check:boundary` rule constrains `src/config/` only — it is the standalone
editor bundle — so nothing objects.

## Data fetching

```ts
["files", "roots"]                    → { roots, limits }
["files", "dir", rootId, path]        → Listing pages (useInfiniteQuery)
```

`useInfiniteQuery`, because the API paginates with an opaque cursor
(`getNextPageParam: (last) => last.cursor ?? undefined`). A directory fetches
automatically while it has more pages **and** has fetched fewer than
`MAX_AUTO_PAGES` (5, so 5000 entries); past that a `ShowMoreRow` appears.
Automatic, because a parent expanding `roms/` wants the folder, not a
scrollbar that asks permission four times — and bounded, because a ROM set in
one directory is real and 50 000 rows is not a page.

There is **no push channel for files**: the SSE event stream carries
`shepherd_api::Event`, and nothing in the file API emits one. So freshness
comes from three places, and it is worth being honest that the tree can be
stale between them:

1. React Query's default `refetchOnWindowFocus` — coming back to the tab
   re-reads what is expanded.
2. The toolbar's Refresh, which invalidates `["files"]` wholesale.
3. Our own mutations, which invalidate exactly what they changed.

Staleness is safe rather than merely likely-to-be-fine, and that is the point
of the API requiring preconditions: a row drawn from a stale listing carries a
stale `etag`, and the write it authorises is refused rather than applied to
something else.

### Invalidation map

| Action | Invalidate |
| --- | --- |
| Upload into `D` | `dir(D)`, and `roots` (free space moved) |
| New folder in `P` | `dir(P)` |
| Rename in `P` | `dir(P)` |
| Move `A` → `B` | `dir(parent(A))`, `dir(B)`, and `forgetSubtree(A)` |
| Delete `X` | `dir(parent(X))`, and `forgetSubtree(X)` |

No optimistic updates in v1. Every one of these is a round trip to a local
disk — the daemon is on the same machine as the files — so the honest render is
a brief spinner on the row, not a guess that has to be rolled back when the
precondition fails.

## Sorting

The API sorts server-side (folders first, then case-insensitive name) **and its
cursor is a position in that order**. Client-side sorting by size or date
therefore cannot be applied to a half-loaded folder without showing a page
boundary that makes no sense.

The resolution follows from the bounded auto-paging above: a folder that has
loaded every page is sorted entirely on the client — any column, either
direction, folders-first as a separate toggle — and a folder that hit
`MAX_AUTO_PAGES` keeps the server's order, greys the other column headers *for
those rows*, and says why in the `ShowMoreRow`. Sorting is per-table, not
per-folder: one sort applies to every expanded folder, as Finder does.

`foldersFirst` is a toggle rather than a constant because the two reasonable
answers disagree — macOS interleaves by default, every file manager a parent
has used on Windows does not — and because with it on, an unsorted truncated
folder and a sorted one look the same, which makes the degraded case less
jarring.

## Drag and drop

Two mechanisms, deliberately, because they are two different browser APIs and
only one of them can be chosen:

| | Mechanism | Why |
| --- | --- | --- |
| Files **from the desktop** | Native HTML5 `dragenter`/`dragover`/`drop` + `DataTransfer` | There is no alternative; only the native events carry `dataTransfer.files`. |
| Rows **within the tree** | `@dnd-kit/core` | Already a dependency, already chosen in the config editor "because it brings keyboard and screen-reader support with it" — the same argument, and here it also brings touch, which native HTML5 drag does not have at all. |

They coexist without conflict: dnd-kit listens on pointer events, the file drop
on drag events, and a drag that carries `DataTransfer.files` never starts a
dnd-kit drag.

### Drop eligibility

One pure function, `canDrop(source, target)` in `dnd.ts`, used to decide both
the highlight and the drop. Never "highlight, then error": a target that cannot
take the thing being dragged does not light up.

| Source | Target | Result | Refused when |
| --- | --- | --- | --- |
| Desktop files | Folder row, or a root row | Upload into it | Target root is `writable: false`; target is not a folder; target row is `usable: false` |
| Desktop **folder** | anything | Refused, with a message | Always in v1 — see "Later" |
| Tree row | Folder row, or a root row | Move | Different root — `POST /files/move` takes one `root` and cannot express a cross-root move at all, so the drop must never be offered; target is the source; target is the source's own parent (a no-op); **target is inside the source's subtree** — moving a folder into itself |
| Tree row | Its own row | — | Always |

The subtree check is the one that bites: `rename("a", "a/b")` fails with
`EINVAL`, which today's error mapping turns into a `500`. The UI must prevent
it, and the API should map it too — see "What the API still owes the UI".

### What a drop looks like

- Hovering a folder row: the row gets a 2px outline in `primary.main` and the
  chevron spins open after ~800 ms (spring-loaded folders, as Finder does), so a
  drag can navigate downward without being dropped first.
- Dragging desktop files over the table but not over any folder: a single
  overlay across the table body — "Drop onto a folder to upload" — rather than
  fifty rows blinking.
- Dragging a row: dnd-kit's `DragOverlay` carries the row's name and icon.

## Mutations, and preconditions as a UI concern

Every write carries the `etag` the row was drawn with. This is the whole reason
the API demands one, and it should surface as *specific* copy rather than a
generic failure — the same move `DeviceConfigSource` already makes for the
policy file.

| Call | Precondition | On `412` |
| --- | --- | --- |
| Upload, new file | `If-None-Match: *` | `ReplaceConfirmDialog`: "There is already a file called X here." → Replace re-sends with `If-Match: *` |
| Upload, replacing a row | `If-Match: "<row etag>"` | `ConflictDialog`: "X changed on the device since this list was loaded." → Reload the folder, and do not retry blind |
| Delete | `If-Match: "<row etag>"`, or `*` for a folder | Same as above |
| Move | — (no precondition on the API) | `409` → `ReplaceConfirmDialog` → retry with `overwrite: true`, files only |

### Error → message

`ApiError` already carries `{status, code, message}`. The table maps `code`,
not `status`, because the code is the stable half:

| `code` | What the row or dialog says |
| --- | --- |
| `too_large` | "This device accepts uploads up to 8 GiB." — checked client-side *before sending*, from `limits`, so a 12 GiB file fails instantly |
| `insufficient_storage` | "Not enough room left on Home (2.1 GB free)." |
| `precondition_failed` | As the table above |
| `conflict` | "Something is already there." |
| `forbidden` | "This device's kiosk user cannot write there." |
| `not_found` | If a root: "That drive is no longer connected." → refetch roots. Otherwise: "That is no longer there." → refetch the parent |
| `bad_request`, `internal` | The daemon's own message, verbatim, in a Snackbar |

`401` is already handled globally: the axios interceptor fires
`UNAUTHENTICATED_EVENT` and `AuthGate` takes over.

## Uploads

```ts
interface Transfer {
  id: string;
  rootId: string; dir: string; name: string;
  file: File;
  status: "queued" | "sending" | "conflict" | "done" | "error";
  sent: number; total: number;
  error?: string;
  abort: AbortController;
}
```

- **Two at a time.** The bottleneck is a local disk and a single daemon; more
  parallelism buys nothing and makes the progress bars lie.
- **Progress** from axios `onUploadProgress` (XHR under the hood), which is why
  the client sends a raw body rather than `fetch` with a stream — and why the
  API takes a raw `PUT` body rather than multipart.
- **Cancel** aborts the request; the daemon removes its `.part` file, and the
  next write into that folder sweeps anything a killed daemon left.
- **The tray persists across navigation** inside the SPA (it lives in a context
  above the page) but not across a reload. A `beforeunload` guard while
  transfers are in flight.
- **Client-side pre-checks**, from `limits` and the root's `free_bytes`: over
  the cap, or over the free space, and the file never leaves the browser.

## Downloads, and the one trap in them

The obvious implementation — `<a href={contentUrl} download>` — works **only
when the SPA is same-origin with the daemon**, because that is when the
`HttpOnly` session cookie is attached. `ConnectionSettings` lets `apiBase` point
at another device, where the credential is a bearer token in `localStorage` that
a plain anchor cannot carry.

So:

```ts
isSameOriginApi()   // localStorage.apiBase is empty — the existing getBase() convention
```

- **Same origin**: a real link. The browser streams to disk, and a 4 GB ROM
  costs no memory.
- **Cross origin**: `axios.get(..., { responseType: "blob", onDownloadProgress })`,
  then an object URL. This *buffers the whole file in memory*, so above ~100 MB
  the UI says so and asks first.

Worth stating in the component that does it, because the failure mode of
getting it wrong is a tab that dies on a large file, and only for the people
using the cross-device mode.

## Mobile, touch, and the keyboard

The parent doing this is as likely to be holding a phone as sitting at a
laptop, and the existing pages are already phone-first
(`useMediaQuery(theme.breakpoints.up("sm"))` for the drawer/bottom-nav split).

- **Below `sm`**: the Size / Kind / Modified columns collapse into a secondary
  line under the name (`2.1 MB · 30 Aug`), the header row disappears, and sort
  moves into a toolbar menu. Row height goes from 36 px to 56 px — a touch
  target, not a table row.
- **Drag is never the only path.** Upload has a toolbar button (a hidden
  `<input type="file" multiple>`); move has `Move to…` in the ⋮ menu, opening
  `MoveToDialog` — the same tree, single-select, folders only, filtered to the
  source's root.
- **Keyboard**, on a `role="treegrid"` with `aria-level`, `aria-expanded` and
  `aria-posinset` per row: ↑/↓ move selection, →/← expand and collapse, `Enter`
  opens (expands a folder, downloads a file), `F2` renames, `Delete` deletes.
  dnd-kit's keyboard sensor gives a keyboard move for free on the rows.

## Row states that are not "a file"

Each of these is a real render, not an afterthought, and each has a row
component or a variant:

| State | Render |
| --- | --- |
| `usable: false` | Greyed, no chevron, no drag, no download. The only enabled action is Delete — which is *why* the API lists these at all. Tooltip: "This link points outside this folder." |
| Root with `writable: false` | A small lock next to the name; no drop target, no New folder, no ⋮ write actions |
| Loading | Skeleton rows at the child depth |
| Error | An inline row with the message and Retry — the node stays expanded |
| Empty | "Nothing in here" |
| Truncated | `ShowMoreRow`: "Showing 5000 of more than 5000. Sorting is by name in this folder." |
| Hidden | Rendered at 60% opacity when `showHidden` is on, so `.config` reads as different from `Books` |

## Performance

No virtualisation in v1, and a shape that makes adding it a wrapper rather than
a rewrite: `visibleRows` is a plain array, `FileRow` is `memo`ised on its node
and row state, and nothing in a row reaches into the tree. If a real device's
`roms/` turns the flat list into thousands of rows and scrolling suffers,
`react-window` wraps the body and nothing else changes. Measuring first is the
point — the alternative is a dependency and a fixed row height bought on
suspicion.

## Tests

The DOM tests in this repo need `afterEach(cleanup)` themselves — Vitest
`globals` are off — and that is where both of the config editor's navigation
bugs were found, so the mount tests earn their place. But most of this is
testable without a browser:

| File | What |
| --- | --- |
| `useFileTree.test.ts` | Pure: expand/collapse, `visibleRows` ordering and depth, `forgetSubtree` pruning after a delete, sort with and without `foldersFirst`, hidden filtering |
| `dnd.test.ts` | Pure: the eligibility matrix above, especially "into its own subtree" and cross-root |
| `useUploads.test.ts` | Queue: concurrency 2, conflict → replace, cancel, the client-side cap check |
| `FilesPage.test.tsx` | Mount: expand a root and see rows; ⋮ → Delete → confirm → the right call with the right `If-Match`; a `412` renders the conflict dialog rather than a generic error |

## What the API owed the UI

Three small things this design found. **All three landed on 2026-09-12**,
before the first component was written; the wire shapes below are what the API
does now, and the reasoning is kept because it is why the fields are shaped
this way.


1. **`usable: false` needed a reason.** The UI has to know whether Delete is
   offered — an escaping symlink can be deleted, a name that is not valid UTF-8
   cannot be addressed at all. Now `unusable: "symlink_escapes" | "name_not_utf8"
   | "special_file" | "not_browsable"`, absent when the entry is fine. The
   fourth value is shepherd's own directories and `~/.ssh`, which the design
   had forgotten are listed too.
2. **`rename` into its own subtree was a `500`.** `EINVAL` fell through to
   `FileError::Internal`. Now a `400` saying a folder cannot be moved inside
   itself, so the UI's own check is a convenience rather than the only thing
   standing between a parent and a stack trace.
3. **Writability was per root, not per directory.** Now `writable` on the
   listing (what delete and rename need — both are permissions on the parent)
   and on directory rows (what an upload into one needs). File rows carry none,
   deliberately: it would look like the answer without being it.

## Staging

Three landings, each independently useful, in this order:

1. **Read-only tree.** Roots, expansion, columns, sorting, download, the ⋮ menu
   with Download only. This is already worth shipping: it is how a parent finds
   out whether the book is on the device. **Built 2026-09-12** — see below.
2. **Writes.** Upload (toolbar + drop), new folder, rename, delete, the
   transfer tray, every dialog. The bulk of the work and all of the
   precondition handling. **Built 2026-09-12** — see below.
3. **Move.** dnd-kit rows, `MoveToDialog`, spring-loaded folders. Last because
   it is the only one with no fallback path today — a parent can already
   download and re-upload to the same effect. **Built 2026-09-12** — see below.

## Later, deliberately not now

- **Multi-select**, and therefore multi-delete and multi-drag. One selection
  keeps the reducer and every dialog's copy simple; the shape leaves room
  (`selected: NodeKey | null` becomes a `Set`).
- **Dropping a desktop folder.** It needs `webkitGetAsEntry`, a recursive walk,
  `mkdir` per level and an ordering guarantee, and it fails halfway in ways
  that need their own resumption story.
- **Previews and thumbnails.** The API deliberately serves every file as
  `attachment` with `nosniff`; a preview is a decision to undo that for some
  types, and it belongs with its own issue.
- **Copy, duplicate, zip download.** Each is a progress-and-partial-failure
  feature the API does not have a route for.
- **A "fix this" affordance from the Diagnostics page.** `EbookBookMissing` and
  `RetroarchContentMissing` both name a path, and the tree could open at its
  folder with the upload button primed. This is the thing that turns a file
  manager into a setup tool, and it wants the read-only tree to exist first.

## Stage 1, as built (2026-09-12)

The read-only tree is in. What landed matches the architecture above, with
three things worth recording because they were decided at the keyboard rather
than here.

- **`useQueries` with `combine`, and paging inside the client.** The flat row
  list means the number of open folders changes between renders, which rules
  out a hook per node — and therefore rules out `useInfiniteQuery`, which has
  no `useQueries` form. So `listDirectory` walks the cursor itself up to
  `DEFAULT_MAX_PAGES`, which is what the infinite hook would have done anyway,
  and leaves `truncated` meaning exactly what the row at the bottom of the
  folder says.
- **The event stream no longer invalidates these queries.** `useEvents`
  invalidated *everything* on every frame the daemon sent; nothing on that
  stream describes the filesystem, so an open tree would have refetched every
  folder on every volume nudge. It now excludes the `["files"]` keys.
- **`isSelectable` is a type guard**, so the keyboard handler narrows a row to
  the two kinds that have an `expanded` field instead of re-checking `kind`.

Verified against a real device, not only in jsdom: `shepherd dev headless` with
a second root configured, then Firefox driven through **geckodriver**. Two
notes for whoever does this next, because both cost an hour:

- `wtype -k <key>` does not reach Firefox — text typing does, named keys do
  not — and the virtual pointer does not reach it either. So the session's own
  input path cannot sign a browser in. WebDriver talks to the page directly,
  and its Add Cookie command can set an `HttpOnly` cookie, which is how a
  session minted over `curl` skips the login form entirely.
- `set_web_password` over the machine token is how that session gets a password
  to mint from, without reading a setup code off the device's screen.

## Stage 2, as built (2026-09-12)

Upload, new folder, rename in place, delete. What changed against the design:

- **The queue lives above the pages.** `UploadsProvider` wraps the whole shell
  in `App.tsx` rather than the Files tab, so a 2 GB video survives a switch to
  the Usage tab — which the design asked for and which turns out to cost three
  lines: the old `App` became `AppShell`, and the new one wraps it with the
  provider and the tray.
- **Permissions are a tested module of their own** (`permissions.ts`). The two
  easy mistakes are worth the file: deleting an entry is a permission on its
  *parent*, and an escaping symlink is deletable — which is the entire reason
  it is listed rather than hidden. Both are now assertions.
- **First attempt always creates.** An upload sends `If-None-Match: *`, and a
  412 becomes a *question* in the tray — a Replace button that resends with
  `If-Match: *` — rather than an error. Silently overwriting a book somebody
  else put there is the one thing this must not do.
- **The caps are checked in the browser.** `max_upload_bytes` and the root's
  `free_bytes` come back with the roots listing, so a file that is too large
  never leaves the page.
- **Rename is inline**, with the stem selected and the extension left alone,
  committing on Enter or blur and guarding against doing both.

### Verified on the device

Driven through geckodriver again (`drive2.py` in the session scratch): new
folder from the ⋮ menu, upload through the hidden input, rename `covers-2026`
to `covers` in place, delete the uploaded file through the confirmation — each
checked against the filesystem afterwards, not only against the page.

Three harness notes, all of them cost time:

- Snap-confined Firefox cannot read a file under `/tmp/claude-*`, so a file
  handed to `<input type="file">` over WebDriver has to live in `$HOME`.
- `clear` on the rename field blurs it, which commits the rename and unmounts
  the input; type into the selection instead.
- The success snackbar says the file's name, so "is it gone" has to be asked of
  the *row* rather than of the page text.

## Stage 3, as built (2026-09-12)

Dragging a row onto a folder moves it; `Move to…` in the ⋮ menu does the same
thing for anybody who cannot drag; a folder hovered mid-drag springs open.

Three things the design did not know:

- **A droppable that registers only once a drag has started is never found.**
  dnd-kit measures its droppables when a drag begins, so the first version —
  which enabled each row's droppable only when the dragged row could go there —
  produced a drag that looked perfect and dropped on nothing. Eligibility now
  splits in two: *could this row ever take a drop* (a writable folder; known
  before any drag, so it is measured) and *would it take **this** one* (the
  pure rule, used for the highlight and checked again at the drop).
- **dnd-kit puts `role="button"` and a `tabIndex` on whatever it makes
  draggable.** On a `<tr>` inside a `treegrid` both are wrong — it destroys the
  row semantics and fights the `aria-activedescendant` caret — so they are
  stripped and the `aria-roledescription` and instructions kept. There is a
  test asserting the rows are still rows, because a dependency bump is exactly
  the thing that would undo it.
- **The rule needed a sentence, not a boolean.** `moveRefusal` returns *why*,
  because the same function decides whether a row lights up and what the
  message says when somebody gets to a drop anyway — a stale tree, the dialog,
  a folder dropped into its own subtree. The `Move` button in the dialog is
  disabled with the reason printed beside it rather than simply dead.

A `409` on a move is a question (replace what is there?) and only for files:
the API will not replace a folder with one, and a recursive merge is not a
thing this does.

### Verified on the device

The drag is a real pointer gesture through WebDriver's Actions API, which
dnd-kit's `PointerSensor` accepts. `notes.txt` dragged onto a shut `Books`:
the folder sprang open under the drag — asserted *before* the release, which
is the whole point of the feature — the drop moved the file, and `Move to…`
moved it back to the top of the place. Each checked against the filesystem
afterwards.

One more harness note, to go with the three from stage 2: a click by text
inside a dialog has to be scoped to `[role="dialog"]`, or it finds the same
name on the tree behind it and WebDriver refuses the click as intercepted.
