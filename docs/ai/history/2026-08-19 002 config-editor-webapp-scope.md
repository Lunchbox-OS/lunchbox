# Config editor — scope

> Status: **scoped, not implemented.** No issue filed yet.

## Prompt

> Scope out a webapp, with roughly the same frontend stack as the existing
> management UI, that builds to an interface for building/editing shepherd
> config.toml files. It should be deployable as a SPA on a static host (GitHub
> Pages or Cloudflare static hosting free tier or similar) with room to build an
> Electron/Tauri app in the future

Then, after a first pass that treated this as forms-over-TOML with comment
preservation deferred:

> I had a graphical editor in mind, but yes, I would like to keep comment
> preservation. Basically you'd be able to add entries and groups and have
> sliders or similar to define availability windows and budgets

Then, after a second pass that put it in a standalone `shepherd-config-editor/`:

> hm, it might be best to build it in shepherd-WebUI with a separate deploy
> target to reduce duplication (I could see it being useful if accessible
> directly there with a new API for updating the config once we have proper
> auth)

Three commitments come out of that, and the rest of this document serves them:

1. **One codebase, two deploy targets.** The editor lives in `shepherd-webui/`
   and builds both into the daemon-embedded management UI and as a standalone
   static SPA.
2. **Direct manipulation is the product.** Availability is a week grid you drag
   on; budgets are sliders; warnings are markers on a timeline.
3. **The document is never regenerated.** Edits are surgical `toml_edit`
   mutations, so comment preservation is a property of the architecture rather
   than a feature that can regress.

## What this is

A graphical editor for `config.toml`, living inside `shepherd-webui`, that ships
in three progressively-connected forms from the same source:

| Form | Where the config comes from | Ships |
|---|---|---|
| **Standalone SPA** | a local file (open / save / download) | first — no daemon, no auth, no new RPCs |
| **In the management UI** | the device, over a new config RPC | once auth is scoped (see below) |
| **Tauri desktop** | local files, natively; optionally push to a device | later |

The reason it's worth building rather than documenting the TOML: the config
surface is large — `schema.rs` is 1012 lines across 25 structs/enums,
`validation.rs` is 1377 lines of semantic rules, `config.example.toml` is 926
lines. Hand-authoring that from docs is where the mistakes are.

And the reason it's worth building *graphically*: several of the trickiest parts
of the config are geometric. Availability is a calendar. Budgets are magnitudes
with an inheritance chain. Warnings are points on a session. Those are all
things you cannot see by reading TOML, and can barely misread on a grid.

## Decisions taken during scoping

| Question | Decision |
|---|---|
| Where it lives | **Inside `shepherd-webui/`**, as `src/config/`, sharing the theme, shell, and helpers. Not a separate package. |
| Deploy targets | **Two rsbuild outputs from one source**: `dist/` (embedded, unchanged for `rust-embed`) and `dist-standalone/` (Cloudflare/Pages). |
| Validation logic | **Compile `shepherd-config` to wasm32 and run the real validator**, in *both* targets. Verified feasible; size measured (below). |
| Comment preservation | **Document-as-truth**: the wasm module owns a `toml_edit::DocumentMut`; the UI applies path-addressed patches. Not merge-on-save. |
| Server-side validation instead of wasm in the embedded build | **Rejected.** The wasm is 864 KB raw / 267 KB gzipped against a 21.7 MB `shepherdd` — ~4%. Not worth a second transport and an offline-broken edit loop. |
| Availability UI | **A 7×24 week grid with draggable blocks**, entry windows over a dimmed group layer. Each block also gets a range slider and `HH:MM` fields. |
| Budget UI | **Non-linear sliders paired with a duration text field**, with inherited values (service default → group → entry) as ghost marks on the track. |
| Grid implementation | **Hand-rolled on pointer events.** Calendar libraries model events on dates, not recurring weekday windows, and are heavy. |
| Config write RPC | **Deferred, and deliberately.** It is an RCE primitive against a blanket auth layer — see "What 'proper auth' has to mean". |
| Media libraries (`movies.toml`) | **Out of scope for v1.** Natural later addition sharing the same shell. |

## Findings from the existing codebase

Verified, not assumed.

### `shepherd-config` compiles to `wasm32-unknown-unknown` today

Its whole dependency tree is pure Rust: `shepherd-util`, `shepherd-api`,
`serde`, `serde_json`, `toml`, `chrono`, `thiserror`, `tracing`. No tokio, no
nix, no filesystem outside `load_config`.

`cargo build -p shepherd-config --lib --target wasm32-unknown-unknown` fails on
exactly one thing:

```
error: to use `uuid` on `wasm32-unknown-unknown`, specify a source of randomness
using one of the `js`, `rng-getrandom`, or `rng-rand` features
```

Adding `js` to `uuid` makes it build clean. The fix belongs in the new wasm
crate as a target-gated dependency, so the feature unifies only into wasm builds
and the Linux binaries are untouched:

```toml
[target.'cfg(target_arch = "wasm32")'.dependencies]
uuid = { workspace = true, features = ["js"] }
```

`parse_config(&str) -> ConfigResult<Policy>` already does no I/O; only
`load_config(path)` touches the disk.

### The wasm module costs about 4% of the daemon binary

Measured, not estimated. A probe crate linking `parse_config`, `toml_edit`'s
document model, and the JSON projection, built `opt-level="z"` + LTO +
`codegen-units=1` + strip + `panic="abort"`:

```
864 496 bytes raw
267 206 bytes gzipped
```

Against context: `shepherd-webui/dist/` is currently 1.7 MB and release
`shepherdd` is 21.7 MB. `wasm-bindgen` glue adds a little; `wasm-opt -Oz` takes
some back.

**This settles the "should the embedded build validate over an RPC instead"
question: no.** Shipping the same wasm in both targets keeps one code path,
keeps the edit loop instant and offline-capable, and costs ~4% of a binary that
already carries 1.7 MB of SPA.

If a lean build is ever wanted, `rust-embed` is already configured with the
`include-exclude` feature and an `#[exclude = ".*"]` line
(`crates/shepherd-http/src/web_assets.rs:16`), so excluding the editor's chunks
behind a cargo feature is a one-line lever — not something to design for now.

### `toml_edit` is already in the tree

`toml` 0.8 is built on `toml_edit` 0.22, compiled for every build today.
Comment-preserving editing costs no new dependency.

### `RawConfig` round-trips, which is the fallback plan

Probe: `config.example.toml` → `RawConfig` → JSON → `RawConfig` →
`toml::to_string_pretty` → `parse_config`:

```
TOML SERIALIZE OK, 10094 bytes (source 36480 bytes)
REPARSE+VALIDATE OK: 15 entries, 1 groups
```

The 36 KB → 10 KB drop is almost entirely comments — which is exactly why
regeneration can't be the primary path. It stays useful as an "export a clean
file" command and as the retreat if the patch engine stalls.

Also noted: defaulted booleans serialize explicitly (`capture_child_output =
false`, `allow_mute = true`). `skip_serializing_if` on the `Raw*` types would
tidy that; low priority now that regeneration isn't the main path.

### Availability semantics the grid must render honestly

From `shepherd-util/src/time.rs:256` and `shepherd-config/src/policy.rs:573`:

- **Days are a bitmask**, bit 0 = Mon … bit 6 = Sun. Presets: `all`/`every`/
  `daily` = `0x7F`, `weekdays` = `0x1F`, `weekends` = `0x60`. Long and short day
  names both parse (`mon` and `monday`).
- **`end` is exclusive** (`time >= start && time < end`).
- **No windows at all means always available**, as does `always = true`. An
  empty grid must render as *fully lit*, or every user will read it backwards.
- **Cross-midnight windows work, with a twist.** `start > end` wraps
  (`time >= start || time < end`), but the day mask is tested against the
  weekday of the *evaluated instant*. So `days = ["fri"], start = "22:00", end =
  "02:00"` does **not** mean Friday night into Saturday — it means Friday
  00:00–02:00 *and* Friday 22:00–24:00, two disjoint chunks in one column. The
  grid should draw exactly that. This is the clearest case for the editor
  existing: near-invisible in TOML, near-unmissable on a grid.
- **`0` means unlimited** for quota and cooldown
  (`seconds_to_duration_or_unlimited`, `policy.rs:1024`), so a slider that
  bottoms out silently at zero is a trap; it needs an explicit detent.

### Entry and group availability intersect

`engine.rs:386` checks the entry's windows and `engine.rs:631` the group's,
independently — both must pass. Effective availability is the intersection,
computable from the config alone, so the grid renders it without a daemon.

Limits cascade similarly: `max_run_seconds` has a service default and
`cooldown_min_session_seconds` cascades service → group → entry
(`policy.rs:87`). The sliders can show all three.

### Auth exists; what's missing is privilege separation

`crates/shepherd-http/src/auth.rs` already accepts two token sources — the
static `[service.management_api].auth_token` and the admin HTTP token minted by
the BLE claim flow — with a documented open mode when neither is configured.
That is more than "no auth".

The gap is that it is a **single blanket layer over the whole API**:
`handlers/mod.rs:32` wraps all of `/api/v1` in one `require_auth`, so any token
that can read usage stats can call anything.

That matters here because **a config write is an arbitrary code execution
primitive by design** — `RawEntryKind::Process { command, args, env, cwd }` runs
whatever it's given. A `set_config` RPC behind today's blanket layer would
silently upgrade every existing management token into a remote shell.

So "proper auth" for this feature specifically means privilege separation — a
scoped capability, an admin-only method class, or a re-authentication step for
config writes — not authentication from scratch. Its own scope, and the right
call to gate on.

### Validation errors are entry-scoped, not field-scoped

```rust
EntryError { entry_id, message } | GroupError { group_id, message }
| DuplicateEntryId(String) | DuplicateGroupId(String)
| InvalidTimeFormat { value, message } | InvalidDaySpec(String)
| WarningExceedsMaxRun { entry_id, seconds, max_run } | GlobalError(String)
```

Enough to badge an activity card; not enough to highlight a field.
`InvalidTimeFormat` and `InvalidDaySpec` don't even carry an entry id.

Graphical editing blunts this considerably: a dragged block cannot produce a
malformed `HH:MM` or an unknown day name, and a warning marker that can't be
dragged past the end of the session bar cannot trip `WarningExceedsMaxRun`.
**Most errors needing field-level attribution become unreachable through the
graphical path.** They stay reachable through the raw TOML pane, where a
line/column is what you want anyway. The refactor stays deferred.

### Unknown keys are silently ignored

No `deny_unknown_fields` anywhere in the crate. A typo'd key does nothing, with
no diagnostic. Cheap to surface once the editor holds the `toml_edit` document.

### Codegen machinery exists and is drift-tested

`crates/shepherd-wire-codegen` generates
`shepherd-webui/src/api/rpc-methods.generated.ts` from schemars JSON Schema, and
`tests/rpc_codegen_drift.rs` fails CI if it goes stale. Adding a `schema`
feature to `shepherd-config` and emitting `RawConfig` types from the same binary
reuses all of it, drift guard included.

## What living in `shepherd-webui` actually saves

Concretely, not hand-wavingly:

- `theme.ts`, `global.css`, the MUI/Emotion setup, and the `App` shell with its
  desktop-drawer / mobile-bottom-nav split.
- `components/Spinner.tsx`.
- **`durationToSecs` and `formatDurationHuman` already exist** in
  `src/api/types.ts` — the `DurationField` at the center of every budget slider
  would otherwise have reimplemented exactly these.
- One `package.json` + lockfile, one `tsconfig.json`, one `rsbuild.config.ts`.
- One entry in `scripts/lib/version.sh` (which version-bumps
  `shepherd-webui/package*.json` by path, with `shepherd version check`
  verifying it in CI) instead of two.
- One npm install and build step in `scripts/lib/build.sh` and CI.

And the thing a separate package could never have: when the config RPC lands,
the editor is already *in* the management UI. No second app to authenticate, no
second deployment to keep in sync with the daemon version.

The cost is one new discipline — see the boundary risk below.

## Why document-as-truth

Two ways to preserve comments; this is why the simpler one loses.

**Merge-on-save.** Keep a plain JS object; at save time reconcile it against the
original document. Less code. But every save is a normalization pass, so
anything the JSON model can't represent gets silently rewritten, the
diff-before-save view shows spurious changes, and the raw TOML pane can only
show a regenerated approximation.

**Document-as-truth.** The wasm module owns the `DocumentMut`; every UI action
is a patch applied to it. Nothing is regenerated, so comments, key order,
inline-vs-standard table style and number formatting survive by construction;
the raw pane shows the *actual document*, live; undo/redo is exact down to
whitespace (the stack is document snapshots — strings, kilobytes each); and the
diff-before-save view is minimal by definition.

The cost is a wasm round-trip per edit, which on a 36 KB document is
microseconds, debounced anyway.

Merge-on-save stays documented as the retreat: it needs no UI change, because
the UI only ever emits patches either way.

## Architecture

```
   shepherd-webui/  ── one source, two targets
   ┌──────────────────────────────────────────────────────────────┐
   │ src/pages/       DashboardPage EntriesPage UsagePage …        │  daemon-coupled
   │ src/api/         axios + react-query + SSE                    │  (embedded target only)
   ├──────────────────────────────────────────────────────────────┤
   │ src/config/      the editor — no import from src/api/         │  both targets
   │   ScheduleGrid   drag blocks on a 7×24 week                   │
   │   LimitsEditor   sliders + duration fields + inheritance      │
   │   WarningTimeline  markers on a session bar                   │
   │   EntryBoard     cards, drag into a group                     │
   │   TokenGraph     nodes and earn edges                         │
   │   Forms          firewall, browser, paths, BLE                │
   │   RawTomlPane    the live document (CodeMirror)               │
   │                                                              │
   │   ConfigSource   ── where the document comes from and goes    │
   │     ├ FileConfigSource    File System Access API / download   │
   │     ├ DeviceConfigSource  get_config / set_config RPC (later) │
   │     └ TauriConfigSource   native fs (later)                   │
   └───────────────────────────┬──────────────────────────────────┘
                    patches    │    projection
                               ▼
   ┌──────────────────────────────────────────────────────────────┐
   │ ConfigDoc (wasm-bindgen) — owns toml_edit::DocumentMut        │
   │   apply · undo · redo · text · view · validate                │
   └───────────────────────────┬──────────────────────────────────┘
                               ▼
   crates/shepherd-config — the real parser, the real 1377-line validator
```

`ConfigSource` is the same abstraction the previous draft called `FileBridge`,
generalized. It is what makes one editor serve a static host, the daemon, and a
desktop app without branching.

### Build targets

`rsbuild.config.ts` becomes a function of `process.env.SHEPHERD_UI_TARGET`:

| | `embedded` (default) | `standalone` |
|---|---|---|
| `output.distPath.root` | `dist/` (unchanged — `rust-embed` and `build.sh` untouched) | `dist-standalone/` |
| `output.assetPrefix` | `/` | `./` (also what Tauri needs) |
| entry | full app; editor lazy-loaded behind a nav item | editor only |
| `ConfigSource` | `DeviceConfigSource`, falling back to `FileConfigSource` | `FileConfigSource` |
| server proxy | `/api` → `localhost:8080` | none |

Two npm scripts (`build`, `build:standalone`). The standalone output **must**
go to a different directory — `rust-embed` takes everything in `dist/`.

The editor is `React.lazy`-imported in the embedded target, and the wasm is
dynamically imported inside it, so the management UI's first paint doesn't pay
for either.

## The Rust side: `crates/shepherd-config-wasm`

A `wasm-bindgen` shim over a document plus a small patch vocabulary. All
semantics stay in `shepherd-config`.

```rust
#[wasm_bindgen]
pub struct ConfigDoc { /* DocumentMut + undo/redo stacks + coalesce key */ }

#[wasm_bindgen]
impl ConfigDoc {
    #[wasm_bindgen(constructor)]
    pub fn open(src: &str) -> Result<ConfigDoc, JsValue>;
    pub fn blank() -> ConfigDoc;
    pub fn from_template(name: &str) -> Result<ConfigDoc, JsValue>;

    pub fn text(&self) -> String;      // the live document, comments intact
    pub fn view(&self) -> JsValue;     // RawConfig projection, for rendering
    pub fn validate(&self) -> JsValue; // Report

    pub fn apply(&mut self, patch: JsValue, coalesce_key: Option<String>)
        -> Result<(), JsValue>;
    pub fn undo(&mut self) -> bool;
    pub fn redo(&mut self) -> bool;

    pub fn effective_windows(&self, entry_id: &str) -> JsValue; // entry ∩ group
    pub fn unknown_keys(&self) -> JsValue;
    pub fn export_clean(&self) -> Result<String, JsValue>;
    pub fn versions() -> JsValue;  // { config_version, crate_version }
}
```

`view()` is `toml::from_str::<RawConfig>(&self.text())` re-serialized to JSON —
trivially correct because it reuses the production parser, and fast enough at
this size not to matter.

`validate()` returns a discriminated report so the UI can tell a syntax error
(line/column, blocks everything) from semantic errors (individually
attributable):

```ts
type Report =
  | { kind: "syntax";   message: string; line: number; column: number }
  | { kind: "version";  found: number; expected: number }
  | { kind: "semantic"; errors: ValidationErrorJson[] }   // [] means valid
```

### The patch vocabulary

Four ops, because the config is a plain tree:

```ts
type Patch =
  | { op: "set";    path: Path; value: Json }
  | { op: "unset";  path: Path }                        // key and its comment
  | { op: "insert"; path: Path; index?: number; value: Json }
  | { op: "move";   path: Path; from: number; to: number }

type Path = string  // "entries[id=minecraft].limits.max_run_seconds"
```

Three rules make this preserve everything:

- **Identity.** `[[entries]]` and `[[groups]]` are addressed by `id`, required
  and validated unique, so reordering or deleting one never misattributes
  another's comments. Other arrays are index-addressed; reordering `warnings` is
  the one operation that can move a comment off its line. Rare, and `move` can
  be left out of phase 1 entirely.
- **Minimal writes.** `set` compares the semantic value first and no-ops if
  unchanged, so a slider dragged away and back leaves the file byte-identical.
- **Style inheritance.** New tables copy an existing sibling's style (inline vs
  standard) when there is one; otherwise standard tables for entries and groups,
  inline for `kind`.

**Coalescing** is what makes sliders usable: `apply` takes an optional
`coalesce_key`, and consecutive patches sharing a key collapse into one undo
step. A drag emits `coalesce_key: "drag:entries[id=x].limits.max_run_seconds"`
and clears it on pointer-up. Without it, one drag is 200 undo entries.

### Workspace placement

Follows the `shepherd-wire-codegen` precedent: a **member but not a
`default-member`**, so a plain `cargo build` never pulls `wasm-bindgen` into
`shepherdd` or the launcher. Reuse that crate's comment block explaining why.

Built with `wasm-pack build --target web`, which needs adding to
`.ci/Dockerfile` and `scripts/deps`, and a step in `scripts/lib/build.sh` before
the npm build.

## The graphical editing model

### ScheduleGrid — the centerpiece

A 7-column × 24-hour grid, 15-minute snap. Blocks are created by dragging empty
space, resized by their edges, moved by their body, deleted by click.

- **One window, many columns.** A `RawTimeWindow` carries a *set* of days, so
  `days = "weekdays"` is one object across five columns. Selecting any highlights
  all five, and dragging an edge moves all five — because that's what the file
  says. Alt-drag splits one day into its own window.
- **Cross-midnight windows render as two bands** in the same column, joined by a
  connector, matching what the engine does. A tooltip explains it rather than
  hiding it.
- **Empty means always.** No windows, or `always = true`, renders the column
  fully lit with a distinct hatch and an explicit label. Never an empty grid.
- **Layers.** The entry's windows are solid; its group's are a dimmed layer
  behind; the intersection — what actually happens — is outlined. Toggleable.
- **Normalization after each edit**: windows with identical `start`/`end`
  coalesce into one with the union of their days, and a day set matching a
  preset uses the preset — but **preserves the user's existing spelling** when it
  still matches. Don't rewrite `daily` to `all`, or `["monday"]` to `["mon"]`.
- **Keyboard equivalents are mandatory**: arrows move the selection, shift+arrows
  resize, and the detail panel always carries `HH:MM` fields and a range slider.
  Drag-only editing is an accessibility dead end, and the panel is faster for
  exact values anyway.

### LimitsEditor — the budget sliders

`max_run_seconds`, `daily_quota_seconds`, `cooldown_seconds`,
`cooldown_min_session_seconds`.

- **Non-linear scale**, fine below an hour and coarse above — the useful range is
  60 s to ~8 h. MUI `Slider` supports this via `scale` + `marks`.
- **Snap** to 1 min under an hour, 5 min over.
- **Always paired with a `DurationField`** built on the existing
  `formatDurationHuman` / `durationToSecs`. The config is entirely in seconds and
  nobody thinks in seconds; a slider alone is infuriating for exact values.
- **Inheritance on the track**: service default and group value as ghost marks,
  effective value called out.
- **An explicit "unlimited" detent** past one end, because `0` means unlimited.

### WarningTimeline

Warning thresholds are points on a session, so they become draggable markers on
a bar as long as `max_run_seconds`, colored by severity. Dragging one past the
end is simply not possible — `WarningExceedsMaxRun` made unrepresentable rather
than merely diagnosed.

### EntryBoard and TokenGraph

Entries are cards in columns by group; dragging a card into a column sets
`group = "..."`. Worth `dnd-kit` here for keyboard and screen-reader support,
even though the grid needs only raw pointer events.

Tokens are a graph: nodes are entries and groups, edges run from each `from`
subject to the gated entry, labelled with `earn_ratio`. Validation already
rejects self-unlocking configurations, so the graph can draw the offending edge
in red. A subject picker plus a rendered, non-editable graph is enough at first.

### Where forms stay forms

Firewall rules, browser policy and URL lists, `env` maps, filesystem paths, BLE
adapter selection, management API binding, Steam interstitial slugs. Lists of
strings and enums with no geometry; inventing widgets would only add clicks.
Stated here so the scope doesn't quietly grow.

## Layout inside `shepherd-webui/src/`

```
  api/            (unchanged — daemon-coupled; off-limits to config/)
  pages/          (unchanged)
  components/     Spinner + anything genuinely shared
  theme.ts  global.css  App.tsx
  config/
    ConfigApp.tsx            entry for the standalone target
    ConfigDocProvider.tsx    holds the wasm handle + projection
    patches.ts               typed patch builders, path helpers
    useProjection.ts         debounced view() + validate()
    sources/
      ConfigSource.ts  FileConfigSource.ts  DeviceConfigSource.ts
    model/
      config.generated.ts    RawConfig types, from shepherd-wire-codegen
      windows.ts             day mask <-> preset, normalize, merge/split
      templates.ts
    components/
      ScheduleGrid.tsx  ScheduleBlock.tsx  WindowDetail.tsx
      LimitsEditor.tsx  DurationField.tsx  WarningTimeline.tsx
      EntryBoard.tsx  EntryCard.tsx  KindEditor.tsx  TokenGraph.tsx
      IssueList.tsx  RawTomlPane.tsx
    wasm/                    wasm-pack output, gitignored
```

**State**: React holds only the wasm handle, the latest projection, and a version
counter. There is no second copy of the config in JS, so nothing to keep in
sync. No zustand, no immer, no reducer — the document *is* the state, and it
lives in Rust.

**Edit loop**: interaction → patch → `apply` → bump version → debounced (~120 ms)
`view()` + `validate()` → re-render. The raw pane reads `text()` directly;
editing it re-opens the document from its text, the one operation that collapses
undo history.

**`windows.ts` deserves real tests.** Day-mask ↔ preset conversion, merge and
split, cross-midnight handling, and preset-spelling preservation are the fiddly
logic here, and they're pure functions. This also means adding a test runner to
`shepherd-webui`, which it currently has none of.

## Deployment

- **Base path.** GitHub Pages project sites serve under `/<repo>/`; Cloudflare
  Pages serves at root; Tauri serves `file://`. The standalone target defaults
  `assetPrefix` to `"./"`, correct for Cloudflare and Tauri, needing only
  `PUBLIC_BASE_PATH` for Pages.
- **Wasm loading.** `wasm-pack --target web` emits a JS wrapper that `fetch`es
  the `.wasm`. rsbuild needs `tools.rspack.experiments.asyncWebAssembly = true`
  and the `.wasm` emitted as an asset. Both hosts serve `application/wasm`. No
  COOP/COEP — no threads, no `SharedArrayBuffer`.
- **Which host.** `origin` is Forgejo (`git.armeafamily.com`), not GitHub, so
  Pages needs a mirror push set up first. **Cloudflare Pages via `wrangler pages
  deploy` from a Forgejo Actions job** (API token secret, same pattern as
  `REGISTRY_TOKEN`) avoids that; direct-upload mode means Cloudflare never sees
  the repo. *Open question below.*

## Room for Electron/Tauri

Unchanged by the move into `shepherd-webui`; Tauri builds the standalone target.
Three rules hold from day one:

1. **All document I/O goes through `ConfigSource`.** No `showOpenFilePicker` or
   `<a download>` in components. `TauriConfigSource` is then one file.
2. **Relative asset paths in the standalone target** — already required by
   Cloudflare, and exactly Tauri's requirement.
3. **`src/config/` never imports `src/api/`.**

Phase 4 is then: add `src-tauri/`, point `frontendDist` at `dist-standalone/`,
implement `TauriConfigSource`, and replace the wasm with `#[tauri::command]`
wrappers holding the same `ConfigDoc` natively — same crate, same patch
vocabulary, smaller bundle, plus native dialogs and drag-and-drop.

## Phases

**Phase 0 — foundations.** `crates/shepherd-config-wasm` with `ConfigDoc`, the
four patch ops, undo/redo, coalescing; target-gated `uuid` fix;
member-not-default-member wiring; `wasm-pack` in `.ci/Dockerfile`,
`scripts/deps`, and `scripts/lib/build.sh`; `schema` feature on
`shepherd-config` and `RawConfig` TS types from `shepherd-wire-codegen` under
the existing drift test; the two-target `rsbuild.config.ts`; a test runner for
`shepherd-webui`.
*Exit: open `config.example.toml`, apply one `set` patch, save — and `git diff`
shows exactly one changed line.* That single criterion proves the
comment-preservation thesis before any UI exists.

**Phase 1 — the graphical MVP, standalone target only.** Entry and group cards
with add/delete/rename; `ScheduleGrid` with the group layer, cross-midnight
rendering, and the empty-means-always state; `LimitsEditor` with inheritance
marks and the unlimited detent; `WarningTimeline`; `KindEditor` for all 7 `kind`
variants; live validation with entry-level attribution; live raw TOML pane;
open/save via `FileConfigSource`; deployed to the static host.
*Exit: author a working config from scratch, and edit a hand-commented one
without disturbing a single comment.*

**Phase 2 — full field coverage.** The form-shaped remainder: per-entry
`internet`, `firewall`, `browser`, `input_compat` + options, `requires_input`,
`volume`, `brightness`, `xwayland_native_resolution`, `confirm_on_close`,
`disabled`/`disabled_reason`; service `steam`, `management_api`,
`ble_management`, `display`, paths. Plus `TokenGraph` and drag-a-card-into-a-group.
*Exit: no field in `schema.rs` is unreachable from the UI.*

**Phase 3 — polish.** Unknown-key lint; structured error paths in
`ValidationError` so raw-pane errors carry line/column; diff-against-original
before saving; `export_clean`; templates seeded from `config.example.toml`; a
**week scrubber** — drag a time cursor and watch the entry list update to show
what would be available then, which `is_available` already answers for free.

**Phase 4 — in the management UI.** Gated on privilege separation in
`shepherd-http` plus new `get_config` / `set_config` RPCs (with
`reload_config()` already there to apply the result). Then: enable the nav item
in the embedded target, add `DeviceConfigSource`, and add a
read-the-current-config-and-diff flow. The editor itself needs no changes.

**Phase 5 — Tauri v2.** `TauriConfigSource`; native `ConfigDoc` commands
replacing wasm; native dialogs and recent files. Optional: `movies.toml` editing,
device push from the desktop app.

## Risks and open questions

| Risk | Mitigation |
|---|---|
| `src/config/` accidentally imports `src/api/`, dragging axios and daemon assumptions into the standalone bundle | The one new discipline the merge costs. `shepherd-webui` has no linter today; cheapest guard is a CI size budget on the standalone bundle plus a grep for `from "../api` under `src/config/`. Adding ESLint with `no-restricted-imports` is the tidier version if a linter is wanted anyway. |
| The patch engine is the main new complexity | Four ops over a plain tree, and the phase-0 exit criterion tests it before any UI exists. Merge-on-save is a known retreat needing no UI change. |
| A `set_config` RPC is an RCE primitive behind a blanket auth layer | Phase 4 is explicitly gated on privilege separation. Do not ship `set_config` before it. |
| Reordering `warnings` can move a comment off its line | Entries and groups are id-addressed and immune. `move` is omitted from phase 1. |
| Cross-midnight rendering looks like a bug | It *is* the engine's behavior. Draw both bands, connect them, explain in a tooltip. |
| Grid normalization rewrites the user's day spelling | Explicit rule: preserve existing preset/spelling whenever it still matches the day set. Covered by `windows.ts` tests. |
| Drag-only editing excludes keyboard users | Keyboard equivalents and a numeric detail panel from phase 1, not as a retrofit. |
| Embedded bundle grows for users who never open the editor | Lazy route + dynamic wasm import, so nothing is transferred until opened. Binary cost is ~4%, measured; `#[exclude]` behind a cargo feature is the lever if that ever matters. |
| Schema drift between TS types and `schema.rs` | Generated by `shepherd-wire-codegen`, guarded by the existing drift test. Phase-2 exit adds a test that every `Raw*` field is referenced under `src/config/`. |

**Open questions** (1 and 2 answered during implementation — see "As built"):

1. **Host.** Cloudflare Pages via `wrangler` from Forgejo Actions (recommended,
   no GitHub dependency), or set up a GitHub mirror and use Pages?
2. **Does the standalone target keep shipping** once the editor is live inside
   the management UI? It stays the only way to author a config for a device you
   haven't set up yet, so probably yes — but it decides whether the two-target
   build is permanent or transitional.
3. **Snap granularity.** 15 minutes is proposed. 5 would suit fine-grained
   bedtime windows; 30 would suit broad-strokes scheduling.

---

## As built (phases 0–2)

Implemented after the scope above was approved, with the instruction *"yes, it
stays. build up to and including phase 2"* — so the standalone target is
permanent, not transitional, and phases 0, 1 and 2 all landed.

### What shipped

**`crates/shepherd-config-wasm`** — the document model. Plain Rust with strings
on its edges (`view`, `validate`, `apply` all take and return JSON strings), so
it builds and tests natively; only the binding layer in `lib.rs` is wasm-gated.
That also meant no `serde-wasm-bindgen` dependency. `wasm-bindgen` and the
`uuid` `js` feature are target-gated to `cfg(target_arch = "wasm32")`, so
neither reaches the native builds — verified with `cargo tree`, which finds zero
occurrences of `wasm-bindgen` or `schemars` in the default build or in
`shepherdd`.

Four patch ops as scoped, with the three preservation rules. 39 tests: 22 unit
plus 17 in `tests/preservation.rs`, which is the suite that earns the crate its
existence.

**Phase 0's exit criterion holds.** `a_single_edit_changes_a_single_line` applies
one `set` to `config.example.toml` and asserts exactly one line differs. Also
covered: every comment survives, a trailing `# one hour` stays on its value,
writing an unchanged value is a no-op that costs no undo step, a drag away and
back leaves the file byte-identical, undo restores the exact bytes, a failed
patch leaves the document untouched, and deleting one entry leaves its
neighbours alone.

**Generated TypeScript mirrors.** `shepherd-config` gained a `schema` feature
(25 `#[cfg_attr(feature = "schema", derive(JsonSchema))]` lines), and
`shepherd-wire-codegen` grew a TS renderer emitting
`shepherd-webui/src/config/model/config.generated.ts` — 842 lines, with the Rust
doc comments carried through as JSDoc. Added to the existing drift test.

**The editor**, in `shepherd-webui/src/config/`: schedule grid, budget sliders,
warning timeline, entry board with drag-to-group, token graph, and the
form-shaped remainder. Phase 2's exit criterion — no field in `schema.rs`
unreachable from the UI — is met, with two deliberate exceptions noted below.

**Two build targets** from one `rsbuild.config.ts` switching on
`SHEPHERD_UI_TARGET`. Sizes, gzipped: the standalone bundle is 26 kB entry +
219 kB shared chunk + 60 kB React + 16 kB CSS + **278 kB wasm**.

The embedded build does **not** route to the editor — see "The tab that should
not have shipped" below — so `dist/` carries none of it and is unchanged from
before this work at 1.7 MB.

**The wasm measurement from scoping held.** 802 kB raw, 278 kB gzipped after
`wasm-opt -Oz`, against the 864 kB / 267 kB the probe predicted.

### Verified end to end

Built the standalone bundle, served it, and drove headless Chrome against it.
The editor boots, loads the validator, and reports "Valid". Seeded temporarily
with `config.example.toml`: all 15 entries render as cards across "No category"
(13) and "Games" (2), with the disabled Terminal greyed out; Tux Math's schedule
draws both windows correctly (weekdays 15:00–18:00 across Mon–Fri, weekends
10:00–20:00 on Sat/Sun); Celeste's limits show the inherited marks on the slider
track with "Not set — Games cap applies (15m)", and its token gate renders the
three source activities and the earn graph with ×0.5 edge labels.

### Decisions taken during implementation

| Question | Decision |
|---|---|
| Grid gestures vs. coalescing | **Grid commits once, on release.** Coalescing exists for sliders, which fire continuously; a drag on the grid is naturally one patch pair, so it needs no gesture key. |
| `set` on a whole table | **Merges key-by-key, then removes what the object omits.** A blunt replace would take every comment inside the table with it. |
| Where the duration helpers live | **Moved to `src/shared/duration.ts`.** `formatDurationHuman`/`formatDuration` were in `src/api/types.ts`, which `src/config/` may not import; `api/types.ts` re-exports them so existing call sites are untouched. `parseDurationHuman` is new. |
| Boundary enforcement | **A 40-line `scripts/check-boundary.mjs`**, not ESLint. The repo has no linter, and adding one to enforce one rule is a poor trade. |
| Validation debounce | **Text updates synchronously, projection and validation at 120 ms.** The raw pane and the save button must never lag an edit; only rendering can afford to. |

### Deliberately not built

- **`vm` and `media` driver arguments** (`args: HashMap<String, Value>`) are
  free-form JSON with no schema to render a form from. The UI says so and points
  at the raw TOML pane.
- **Everything in phase 3**: the unknown-key lint, structured error paths, the
  diff-before-save view, `export_clean`, templates, and the week scrubber.
- **Phase 4's `DeviceConfigSource`**, which stays gated on privilege separation
  in `shepherd-http`. `ConfigSource` is in place and documents the constraint;
  `FileConfigSource` is the only implementation.

### Follow-up: the tab that should not have shipped

The first cut wired a `Config` entry into the management UI's `NAV` array
unconditionally, behind a `React.lazy` import. That contradicted the phase list,
which gates the in-management-UI integration on privilege separation, and it was
caught by the question *"when is the editor visible on the management webui"* —
the answer being "always, right now".

Two things had been conflated. `DeviceConfigSource` genuinely was deferred, so
the editor could not read or write the device's config; but the **tab** was
visible, and with `FileConfigSource` as the only implementation it would have
edited a file on whatever computer was doing the browsing. In a device's own web
UI that reads as "edit this device's configuration" while doing nothing of the
sort. No security exposure — the tab sits behind the same blanket `require_auth`
as everything else and has no RPC that can write a config — purely misleading.

Resolved by removing the route entirely rather than gating it on a flag.
`src/App.tsx` only ever runs in the embedded build, so a condition there would
always be false; a comment at the point where the route would go records why it
is absent and what to do when `DeviceConfigSource` lands.

Two consequences worth knowing:

- The editor's chunks and its wasm validator are no longer emitted into `dist/`,
  so they are no longer compiled into `shepherdd`. Measured: `dist/` drops from
  2.8 MB back to **1.7 MB**, exactly what it was before this work, and
  `find dist -name '*.wasm'` returns nothing. **The ~4% binary-size figure from
  scoping no longer applies** — the cost to the daemon is currently zero. It
  comes back when the tab does.
- `dist-standalone/` is untouched: still 802 kB / 278 kB of wasm, and verified
  in a browser after the change.

### Follow-up: `shepherd dev webui`

Added after the phases landed, on the request *"add a command to just run the
dev server to the shepherd script"*. It lives under the existing `dev`
namespace (`scripts/lib/build.sh`, dispatched from `scripts/shepherd`) rather
than as a new top-level command, since `shepherd dev` already collects the
interactive development entry points.

    shepherd dev webui                 # management UI, /api proxied to :8080
    shepherd dev webui --standalone    # config editor alone, no daemon
    shepherd dev webui --wasm          # rebuild the validator first
    shepherd dev webui -- --port 3001  # anything after -- goes to rsbuild

It builds the wasm validator when `src/config/wasm/` is absent and runs
`npm install` on a cold checkout, then hands off to rsbuild in the foreground.
The wasm is needed by **both** targets, not just the standalone one: the
management UI imports the editor lazily, but rspack still resolves the module
graph eagerly, so a missing artifact fails the dev server either way.

Verified: both targets serve HTTP 200 (`--standalone` and default), `-- --port`
passthrough reaches rsbuild, and deleting `src/config/wasm/` before starting
rebuilds it and then serves.

### Follow-up: hosting

Open question 1 resolved to **Cloudflare Pages**, Direct Upload, at
`shepherd.armeafamily.com`, deployed from the `config-editor` job in
`release.yml`. Direct Upload rather than a git integration because the repo is
on Forgejo and not mirrored; Cloudflare therefore needs no repo access, only an
API token.

Open question 2 — whether the standalone target keeps shipping once the editor
is reachable from the management UI — is answered **yes, permanently**, which is
what makes the hosting decision worth making at all.

Two deploy-shape decisions taken with it:

| Question | Decision |
|---|---|
| Trigger | **Version tags, not every push to main.** The editor renders a `config_version` that ships with the daemon; an editor ahead of the release would offer fields the installed version cannot read. |
| Prerelease tags | **Skipped**, mirroring what the apt registry already does — an RC should not replace the editor people are using. The bundle is still *built* on every trigger, so a dry run exercises wasm-pack and the npm build. |

Host requirements turned out to be unusually thin, which is worth recording
because it is the part people assume is hard:

- **No rewrite rules.** The editor has no router and never touches
  `history`/`location`, so the usual SPA fallback is unnecessary.
- **No COOP/COEP.** No threads, no `SharedArrayBuffer`.
- **`application/wasm` is a nice-to-have, not a requirement.** wasm-pack's glue
  tries `instantiateStreaming` and falls back to `WebAssembly.instantiate` on a
  Content-Type mismatch (`shepherd_config.js:377`), with a console warning.
- **CSP must allow `'wasm-unsafe-eval'`** in `script-src`. This is the one that
  actually breaks the page, and the one a strict default header set gets wrong.
- `assetPrefix` is already `"./"`, so a subdomain root needs no rebuild config;
  `PUBLIC_BASE_PATH` exists only for subpath hosting.

### Notes for whoever picks this up

- `output.distPath` in `rsbuild.config.ts` is **relative to the working
  directory**. Running `rsbuild build` from the repo root cleans the repo's own
  `dist/` (polkit, systemd, udev, fdroid). The build scripts always `cd` into
  `shepherd-webui` first; do the same by hand.
- MUI 9 removed the system props (`alignItems`, `justifyContent`, `flexWrap`,
  `display`) from `Stack` and `Typography` — they go in `sx`, which is what the
  existing pages already do. Also: `inputProps` moved under `slotProps`,
  `Autocomplete`'s `renderTags` became `renderValue`, and the icon is
  `DeleteOutlined`, not `DeleteOutline`.
- The one behaviour worth re-reading before touching the grid is the
  cross-midnight rule in `crates/shepherd-config-wasm/src/windows.rs`. It has a
  test named after what it does.
