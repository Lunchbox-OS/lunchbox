# Management-based config editor improvements (issue #186) — scope

> Status: **scoped, not built.** This is the survey and the argument. The
> judgement calls it turns on are listed under "Decisions"; everything above
> them is the reasoning that produced the options.

## Prompt

> scope out #186

[Issue #186](https://git.armeafamily.com/albert/shepherd-launcher/issues/186),
*"Management-based config editor improvements"*:

> Building on #185, once the config editor is built-in, the built-in version
> could use some quality-of-life improvements:
> * Steam activity autosuggestion: make the ID selector an autocomplete that
>   lists the actually-installed games
> * File picker for things like picking a media library, selecting an ebook
>   file, tying a RetroArch activity's ROM
> * RetroArch backend autodetection: *suggest* viable emulator backends based
>   on the selected ROM from above

No comments, no labels. #185 landed in `main` on 2026-09-07 as PR #189, so the
premise ("once the config editor is built-in") is satisfied.

All three items are the same shape: **the editor is now running on the machine
it is configuring, and it still asks the administrator to type facts that
machine already knows.** `app_id = 504230`, `core = "mgba"`, and
`content = "~/Games/retroarch/pokemon-firered.gba"` are all typed blind today,
verified only by the launcher failing later.

## What exists today

### The three fields, as they are typed now

All in `shepherd-webui/src/config/components/KindEditor.tsx`, all plain
`TextField`s:

| field | line | validated as |
|---|---|---|
| `steam.app_id` | `KindEditor.tsx:217` | `app_id > 0` (`validation.rs:388`) |
| `retroarch.content` | `KindEditor.tsx:391` | non-empty, absolute or `~/`-rooted (`validation.rs:880`) |
| `retroarch.core` / `core_path` | `KindEditor.tsx:402` | exactly one of the two is set |
| `media.library` | `KindEditor.tsx:252` | non-empty; may be a path *or* a YouTube URL |
| `ebook.book` | `KindEditor.tsx:484` | non-empty, absolute or `~/`-rooted (`validation.rs:800`) |

Nothing in the editor checks that the path exists, that the core is installed,
or that the app is on the machine. The device does check, twice, but only
afterwards: `retroarch::missing_core` /`missing_content` raise diagnostics
(`crates/shepherdd/src/diagnostics.rs`), and a launch fails. The whole of this
issue is moving that knowledge earlier, into the field.

### The hard constraint: `src/config/` may not talk to a daemon

`shepherd-webui/scripts/check-boundary.mjs`, run by CI, fails the build if
anything under `src/config/` imports `src/api/`, `axios`, or
`@tanstack/react-query`. The reason is in the script: that tree also builds into
the **standalone** static bundle, which has no device to ask.

So none of these three features can be a `fetch` inside `KindEditor`. They have
to arrive the way the config document itself does — through an injected seam.
#185 already built the precedent: `ConfigSource` is declared in
`src/config/sources/`, and its device implementation lives *outside* the
boundary in `src/sources/DeviceConfigSource.ts`. This issue needs a second seam
of the same shape, for reading facts rather than the document.

`KindEditor`'s props are `{ kind, onChange }`, and two vitest suites
(`defaults.test.tsx`, `sponsorblock.test.tsx`) render it directly with nothing
else. Whatever the seam is, its absence has to be the current behaviour — a
plain text field — or those tests break and the standalone bundle breaks with
them.

### What the device actually knows — verified, not assumed

**RetroArch cores.** Two separate facts, and the feature needs both.

- *Installed* cores are `.so` files under `retroarch::core_search_dirs()`:
  `~/.config/retroarch/cores`, the distro's `/usr/lib/<multiarch>/libretro`,
  and `$SHEPHERD_LIBRETRO_DIR` when `--trust-environment` is on. On this dev box
  that is exactly one file, `mgba_libretro.so`.
- *What a core can open* comes from the libretro `.info` catalog. This box has
  **316** of them in `/usr/share/libretro/info`, which is where
  `~/.config/retroarch/retroarch.cfg` points `libretro_info_path`, and which
  `dpkg -S` attributes to the `libretro-core-info` package.
  `shepherd-admin apps install retroarch` installs that package unconditionally
  (`scripts/lib/admin.sh:955`: `packages=(retroarch retroarch-assets
  libretro-core-info)`), so on any device set up the shepherd way, the catalog
  is present whether or not any core is.

  Each file is TOML-ish `key = "value"` lines. `mgba_libretro.info` carries
  everything the suggestion needs:

  ```
  display_name = "Nintendo - Game Boy Advance (mGBA)"
  supported_extensions = "gb|gbc|gba"
  corename = "mGBA"
  systemname = "Game Boy/Game Boy Color/Game Boy Advance"
  categories = "Emulator"
  ```

  So "which cores open a `.gba`" is a real query against real data, not a table
  we would have to invent and maintain.

- A curated list of the cores shepherd knows how to *install* already exists —
  in bash. `RETROARCH_CORES` (`scripts/lib/admin.sh:809`) is 14 rows of
  `<core>:<system it runs>`, deliberately distro packages only. Rust has its own
  half of the same knowledge: `CORE_ALIASES` (`retroarch.rs:237`), the 12 cores
  whose apt package name and shared-object name disagree (`beetle-psx` ships
  `mednafen_psx_hw_libretro.so`). Turning "this core would open your ROM but
  isn't installed" into a runnable `shepherd-admin apps install retroarch
  beetle-psx` needs both halves, which currently live on opposite sides of the
  language boundary. That is decision 4.

**Steam.** Nothing in the tree parses a Steam library today — the only reference
to Steam's data directory is `steam_interstitial.rs:268`, which reaches for
`~/snap/steam/common/.local/share/Steam/.cef-enable-remote-debugging`.

The important constraint is that `type = "steam"` drives **one** Steam:
`steam_preload_argv` (`process.rs:283`) is literally `snap run steam -silent`,
and `config.example.toml:466` says `sudo snap install steam`. So enumerating a
native or flatpak Steam's games would offer the administrator games this entry
kind cannot launch. The list should come from the snap's root and say so.

Inside that root the data is `steamapps/appmanifest_<appid>.acf` — Valve's VDF
key/value text, carrying `appid`, `name`, `installdir` and `StateFlags` — plus
`steamapps/libraryfolders.vdf`, which names any additional library roots (an
external drive). Both are private formats: stable in practice, never promised.

There is a second, weaker source already in use: `find_steam_icon`
(`icon.rs:120`) reads `~/.local/share/applications/steam_<app_id>.desktop`.
That file only exists when someone asked Steam for a desktop shortcut, so it is
a good place to get an icon for an app you already know about, and a bad list.

**And a third, which is not a file at all: we can ask the Steam client.**
`steam_interstitial.rs` already speaks the Chrome DevTools Protocol to Steam's
own CEF UI on loopback — `GET /json` to enumerate page targets, then a
WebSocket `Runtime.evaluate` per target, with `tokio-tungstenite` already a
dependency and a 3s per-target timeout. It exists to click "Play anyway" on a
cloud-sync modal, but the primitive it built is general: **arbitrary JS
evaluated inside the running Steam client.** The client's own JS context holds
the app store — display names, app types, installed state — which is a strictly
better list than the manifests: it knows a *game* from a tool, a runtime and a
DLC, where a manifest scan has to guess by appid and name.

Four things gate it, and they are the whole argument:

- **`evaluate_on_target` returns `/result/result/value` as a string**, so a
  query returning `JSON.stringify(...)` needs *no transport change at all*. What
  it does need is `title`/`url` on `CefTarget` (currently only `type` and
  `webSocketDebuggerUrl` are deserialised) so the query goes to the client's
  shared JS context rather than being broadcast at every page.
- **CEF debugging is on by default, but only where Steam already matters.**
  `auto_dismiss_interstitials` unset means the safe default set
  (`policy.rs:512`), so `ensure_cef_debug_enabled()` runs — but only from
  `preload_steam`, and `preload_steam` is called only when the policy
  **already has a Steam entry** (`main.rs:1465`). The flag is read by Steam at
  startup.
- So the client is reachable exactly when the parent is editing their *second*
  Steam entry, and unreachable when they are adding their *first* — which is
  the case the autocomplete exists for.
- **The picker must not turn the endpoint on.** `preload_steam`'s own comment
  states the position: "the endpoint is a control surface we don't open
  otherwise." Enabling CEF debugging (and restarting Steam) to populate a
  dropdown would trade a standing control surface for a convenience, and would
  be this project deciding the opposite of what it already decided.

The client's JS API is also undocumented and moves between Steam updates — the
same bet the interstitial signatures already make, and it fails the same way:
softly, into an empty result and a plain text field.

**None of the Steam half is verified.** No Steam on this box, and the snap is a
multi-gigabyte install that then wants an account before it shows a library, so
the manifest layout and the client's JS shape here are knowledge, not
measurement — unlike everything in the RetroArch section, which was driven.
That gap is the first thing implementation should close, on a device that has
Steam.

**What a ROM is — measured, not assumed.** The issue says "based on the selected
ROM", and the obvious reading is "by its extension". Two better detectors exist
on the device already, and both were driven here against synthesised fixtures
(`file` 5.46, RetroArch 1.22.2) rather than reasoned about.

`file(1)` identifies the *format*, from the header, ignoring the name:

| fixture | `file --mime-type` |
|---|---|
| Game Boy (logo at `0x104`) | `application/x-gameboy-rom` |
| Game Boy Color (`0x143 = 0xC0`) | `application/x-gameboy-color-rom` |
| Game Boy Advance (logo at `0x04`) | `application/x-gba-rom` |
| NES (`NES\x1a`) | `application/x-nes-rom` |
| Nintendo 64 (`80371240` **and** the clock rate at `0x04`) | `application/x-n64-rom` |
| Mega Drive (`SEGA` at `0x100`) | `application/x-genesis-rom` |
| Master System / Game Gear (`TMR SEGA`) | `application/x-sms-rom` |
| **SNES** (`.sfc`, internal header at `0x7FC0`) | `application/octet-stream` |
| **PS1** (`.bin` / `.cue`) | `application/octet-stream` / `text/plain` |
| **anything zipped** | `application/zip` |

So magic covers most of the 8- and 16-bit consoles and *fails exactly where the
extension is also weakest*: SNES has no magic number at all, a PS1 disc is a
`.bin` beside a text `.cue`, and an archive hides everything. The magic itself
is a dozen header checks — worth doing **in-process in Rust**, not by shelling
out to `file`: no subprocess, no libmagic, and the same twenty lines can look
inside a zip's central directory, which gives the inner file's name *and* its
CRC32 without decompressing anything.

`retroarch --scan=FILE` is a different tool entirely: it identifies the **game**,
by CRC32, against the 142 `.rdb` databases in `/usr/share/libretro/database/rdb`
(the `retroarch-data` package, a dependency of `retroarch`). Verified end to end
by forging a file whose CRC32 matches a real database entry:

```
$ retroarch -c <patched skeleton> --scan=/…/mystery.gba
[INFO] [Scanner] Added 1 entries to "Nintendo - Game Boy Advance.lpl".

  "label":   "xniq (World) (Proto) (GBA Jam 2021)",
  "db_name": "Nintendo - Game Boy Advance.lpl",
  "core_name": "DETECT"
```

Five things that says, all of them load-bearing:

- It **runs headless** — no display, no core loaded — in about a second, exit 0.
- It ignores the filename: `mystery.gba` came back as the real game's name. It
  also **descends into archives** (`mystery.zip#mystery.gba` matched identically),
  which is the one thing extension matching can never do.
- `db_name` is the **system**, and it is the *same string* the `.info` catalogue
  joins on: mgba's `database = "Nintendo - Game Boy|Nintendo - Game Boy
  Color|Nintendo - Game Boy Advance"`. So system → cores is a join, not a
  heuristic — whichever detector produced the system.
- `core_name` is `"DETECT"`. RetroArch does **not** pick a core; that step is
  still ours.
- It matches **known dumps only**. A synthetic-but-well-formed GBA ROM got
  `No match for: … ( A0FA4E40)`. Homebrew, ROM hacks, translations, trimmed or
  overdumped dumps — a good share of what a household legitimately owns — get
  nothing at all.

And it is fiddly to drive. Ubuntu's skeleton `/etc/retroarch.cfg` points
`content_database_path` at `~/.local/share/retroarch/rdb`, which does not exist,
while the packaged databases are in `/usr/share/libretro/database/rdb` — so out
of the box **`--scan` matches nothing**. Passing the key through
`--appendconfig` did not take, and a hand-written minimal `-c` config did not
either; what worked was the skeleton config copied with two keys patched. Every
run also creates config and playlist state under `HOME`, which is precisely what
`retroarch.rs` promises never to touch ("The user's `retroarch.cfg` is never
edited"), so it would need a scratch `HOME` of its own.

**The filesystem.** shepherdd runs as the kiosk user, and `~` in a config path is
expanded at launch relative to *its* `HOME`. So a browse endpoint served by
shepherdd sees exactly the filesystem the launcher will see when it opens the
file — which is the property that makes a picker worth having rather than
merely convenient.

### The transport, and what a new method costs

Settled ground from #185, still true:

- `#[management_rpc]` turns **every `async` method** on `ManagementService`
  into a `dispatch_json` arm, and BLE serves that same dispatch against a
  16 KiB `MAX_FRAME_BYTES` (`crates/shepherd-ble/src/protocol.rs:45`). Non-async
  trait methods are skipped, which is how `read_policy`/`write_policy` stay off
  BLE.
- Adding a trait method also regenerates **10** client mirrors
  (`cargo run -p shepherd-wire-codegen --bin rpc-codegen`), three of them Kotlin
  files in `companion-android/`, with a CI drift check that fails until they are
  committed.
- Payload types do not have to ride on the trait to get a generated TypeScript
  mirror: `WireTypes` in `crates/shepherd-wire-codegen/src/wire_schema.rs` roots
  types explicitly, exactly as it already does for `AudioOutputRecord` and
  `NetworkStatusView`.
- `list_audio_outputs` (`service.rs:137`) is the standing precedent for "the
  management API answers a question about this device's hardware", backed by a
  pluggable controller rather than by the service itself.

Sizes, for the three payloads this issue needs: a Steam library of 50 games is a
few KB; the `.info` catalog reduced to (name, display name, extensions, system)
is on the order of 20–30 KB for 316 cores; **a directory listing is unbounded.**
Only the third is disqualified from BLE on size alone — but see decision 1 for
why the other two may not want to be there either.

## What has to be built

Three server-side facts, one client-side seam, three field treatments.

### 1. A device-facts seam in the editor

Declared in `src/config/sources/` beside `ConfigSource` — a type only, no
implementation, no import of `src/api/`:

```ts
export interface DeviceFacts {
  steamApps(): Promise<SteamApp[]>;
  retroarchCores(): Promise<CoreCatalog>;
  browse(path: string): Promise<DirListing>;
}
```

Published through a React context whose default is **`null`**, so:

- the standalone bundle, which has no device, renders today's text fields;
- the two vitest suites that render `KindEditor` bare keep passing untouched;
- a field never *requires* a suggestion — free text stays valid everywhere,
  which matters more than it sounds (see Risks).

The implementation lives outside the boundary in `src/sources/DeviceFacts.ts`,
against the new routes, and does its own memoisation there — react-query is
forbidden inside `src/config/`, and the editor should not grow a cache of its
own. `ConfigApp` takes it as a prop next to `source` and provides the context.

### 2. `GET /api/v1/steam/apps`

The installed games of the Steam **snap**, from two sources with one shape,
merged on app id, each answering the case the other cannot:

**Baseline — the manifests on disk.** `appmanifest_*.acf` across
`libraryfolders.vdf`'s roots: app id, name, install state, library root. Works
with Steam not running, with CEF debugging off, on a device adding its first
Steam entry — the bootstrapping case, and the one that decides this cannot be
CDP-only.

- Filter to fully-installed real games: `StateFlags & 4`, and drop the
  infrastructure appids (Steamworks Common Redistributables, the Steam Linux
  Runtimes, Proton) — installed apps that are not games. A manifest scan can
  only guess at this by appid and name.
- Parse defensively: one malformed manifest must not fail the endpoint. A small
  hand-rolled VDF reader with fixtures beats a dependency here (decision 5).

**Enrichment — the running client, when it is already reachable.** One
`Runtime.evaluate` against the shared JS context through the existing CDP
client, returning `JSON.stringify` of `{appid, name, type, installed}`. This is
what turns the guesswork above into an answer: the client knows a game from a
tool from a DLC. Merged over the baseline by app id; never required, never
awaited past the existing 3s timeout, and it **must not** enable CEF debugging
or restart Steam to get itself an answer (decision 8).

Both live in `shepherd-host-linux` next to the rest of the Steam machinery,
behind one `HostAdapter` method with an `Ok(vec![])` default, the way
`list_windows` is — so the merge, the filtering and the fallback are one
testable function and the HTTP layer sees a single list.

### 3. `GET /api/v1/retroarch/cores`

The merge of *installed* (`core_search_dirs()`) and *catalogued*
(`libretro_info_path`, default `/usr/share/libretro/info`):

```
{ core: "mgba", display_name: "Nintendo - Game Boy Advance (mGBA)",
  system: "Game Boy/Game Boy Color/Game Boy Advance",
  extensions: ["gb","gbc","gba"], installed: true, install_hint: "mgba" }
```

`install_hint` is present only for cores `shepherd-admin apps install retroarch`
can actually install — anything else would be a lie printed in the UI.

Caching: 316 file reads is not free. Parse under `spawn_blocking`, cache in the
daemon keyed on the info directory's mtime.

The suggestion itself belongs in Rust with unit tests, not in the browser, and
it is a ladder rather than a lookup — each rung is a way of naming the *system*,
which is then joined to cores through the `.info` `database` field:

1. **Header magic**, in-process: a dozen checks covering GB/GBC/GBA/NES/N64/
   Mega Drive/SMS, and for a `.zip`, the central directory's entry name (and
   CRC32) without decompressing. Beats the extension whenever the file is
   mis-named or archived.
2. **Extension**, via `supported_extensions`, for everything magic cannot see —
   SNES, PC Engine, PS1. This is the rung the original scope had.
3. Nothing. Say so plainly and show the unfiltered core list; a wrong
   suggestion is worse than an honest "I can't tell".

`retroarch --scan` sits outside this ladder, because what it adds is not the
system but the **game's name** — a strong default for the entry's `label` and
its search for cover art. It is also the rung that fails on homebrew. Decision 3.

### 4. `GET /api/v1/files?path=…`

A **read-only, metadata-only** directory listing: name, dir/file, size, mtime,
symlink flag. Never file contents. Dirs first, capped (1000) with an explicit
`truncated` flag, `spawn_blocking`, and errors reported per-path rather than
failing the request.

Two details that are not incidental:

- **It must speak `~/`.** `content` and `book` are validated as "absolute or
  starting with `~/`" (`validation.rs:884`, `:804`), and `~/Games/…` is the
  spelling that survives a home-directory move. A listing under `HOME` should
  hand back the `~/`-rooted path, not the expanded one.
- **Where it starts.** `HOME`, mounted media (`/media/<user>`, `/mnt`, `/srv`),
  and every directory the current config already references — the last is free,
  since the editor is holding the document.

### 5. The three field treatments

- **Steam app id** → MUI `Autocomplete freeSolo`, options `name — app_id`, the
  typed value still accepted verbatim.
- **RetroArch content** → path field + picker, filtered by "extensions some
  catalogued core accepts" (which is feature 3's data doing double duty).
- **RetroArch core** → `Autocomplete` grouped *Installed* / *Available to
  install*, narrowed by the current `content`'s extension, with a "show all"
  escape and the install command shown for an uninstalled pick.
- **`media.library`, `ebook.book`** → the same path field, filtered to
  `toml|m3u|m3u8` and `epub|pdf|cbz|cbr|cb7|cbt|djvu|djv` respectively. The
  media field must keep accepting a YouTube URL, so the picker is an adornment
  on a text field, never a replacement for it.

One `<PathField>` component, taking an extension filter — which is also what
makes decision 6 (whether `process.command`, `cwd`, entry `icon`, the service's
`data_dir`/`log_dir`, and the TLS `cert`/`key` get one) nearly free.

## Risks and things that will bite

| | |
|---|---|
| **A missing suggestion is not an invalid value.** A ROM on an unmounted drive, a game not installed yet, a device being configured before its software is. Every field has to stay free-text, and the UI must never style "not in the list" as an error. | |
| **The browse endpoint is a read primitive over the kiosk user's whole home**, and today's admin surface has no such thing. The counter-argument is #185 decision 1: this caller can already `PUT` a config containing `RawEntryKind::Process { command }`, which is arbitrary execution at that same uid. Directory *names* are strictly inside that envelope — but "strictly inside" is an argument, not an absence of change, and it should be made deliberately. | decision 2 |
| **Family privacy, not just security.** The parent holding the admin session is authorised; the child whose file names are on the parent's phone screen did not think of it that way. Bounded by metadata-only. | |
| **Steam's `.acf`/`.vdf` are private formats, and its client JS API is a moving one.** Both are bets the tree already makes — the interstitial signatures are the same wager — and both must fail the same way: an empty list and a plain text field, never an error page. | decision 8 |
| **The CEF endpoint is a control surface.** It is on by default on a device that already has a Steam entry, and absent on one that does not. A picker that opened it, or restarted Steam to get itself an answer, would reverse a decision `preload_steam` has already made deliberately. | decision 8 |
| **Suggestions go stale.** Install a game, or `apps install retroarch snes9x`, with the editor open. Needs a refresh affordance, or fetch-on-open rather than fetch-once. | decision 7 |
| **The extension is weakest exactly where help is most needed.** Dozens of cores claim `zip`; a `.cue`/`.bin` pair is one game in two files; SNES has no header magic and PS1 has no magic worth the name. The zip half is answered by reading the archive's central directory; SNES and PS1 fall back to the extension and must degrade to "cannot narrow" rather than to a wrong core. | decision 3 |
| **A slow or huge directory** — a network mount, a 20 000-file ROM set. Cap, `spawn_blocking`, and a timeout. | |
| **Two catalogues, one truth.** `RETROARCH_CORES` in bash and `CORE_ALIASES` in Rust already describe the same 14/12 cores from different angles. A third copy for install hints is how the three drift. | decision 4 |
| **Phone form factor.** #185 accepted that the editor is desktop-shaped on a phone. A file browser is the one part of this that is *better* on a phone than typing — worth keeping list-shaped rather than tree-shaped. | |
| **Bundle cost** is small: MUI `Autocomplete` is already a dependency, and all of this lands in the editor's already-lazy chunk. | |

## Verification

- **Unit, no filesystem**: VDF and `.info` parsing over fixtures; the
  identification ladder (header magic, zip central directory, extension
  fallback, and the "cannot tell" rung) over synthesised ROM headers — the same
  fixtures this scope was measured with, which are a few dozen bytes each and
  contain no game data; the system→core join; path normalisation, `~`
  round-tripping, and traversal; the listing cap.
- **`crates/shepherd-http/tests/api.rs`**: 401 without a credential on all
  three routes, shapes, and that the browse route returns metadata only.
- **vitest**: `DeviceFacts` = `null` renders exactly today's fields (this is the
  regression that protects the standalone bundle); the autocomplete accepts a
  value that is not in its options.
- **Headless dev session** (`./scripts/shepherd dev headless` → `dev shot` →
  `dev stop`, per the `headless-dev` skill): RetroArch, the 316-file `.info`
  catalog and one installed core (`mgba`) are all present on this box, so the
  whole RetroArch half — including "suggested, not installed" — verifies here
  end to end. The file picker verifies here too.
- **Steam cannot be verified on this box.** `snapd` is here (this box runs the
  Firefox snap) but Steam is not, and the snap is multi-gigabyte and wants an
  account before it has a library to list. Fixtures cover the manifest parser;
  a `--trust-environment`-gated root override, following the
  `SHEPHERD_RETROARCH_ROOT` precedent from #144, covers the scan. Two things
  genuinely need a device with a logged-in Steam, and should be done **first**
  rather than last, because they are the assumptions the design rests on: that
  the manifest layout is what we think, and that the client's JS context
  answers the app-store query the way we think. Both are currently knowledge
  rather than measurement.

## Decisions

To be answered before implementation.

### 1. Transport: HTTP routes, or `ManagementService` methods?

The browse endpoint is unbounded and cannot be a BLE-served async trait method.
For the other two the question is real: the trait buys generated TS types and a
companion mirror; it costs three regenerated Kotlin files for a surface the
companion cannot use, since #185 decided the companion does not edit configs.

Recommendation: **all three as HTTP routes**, matching `/api/v1/config`, with
their payload types rooted in `WireTypes` so TypeScript is still generated
rather than hand-written.

### 2. How far may the browse endpoint see?

Whole filesystem as the kiosk uid (metadata only, read only), or an allow-list
of roots? Recommendation: **whole filesystem, metadata only**, on the #185
decision-1 argument — with the privacy consequence written down rather than
discovered.

### 3. Archives and multi-file content

Superseded by what the measurements found. The question is no longer "peek or
not" but **how far up the ladder to build**:

- **Rungs 1 and 2** (header magic in-process, then extension) — recommended,
  and cheap. A zip is read through its central directory: entry name for the
  inner extension, no decompression, no new dependency. This subsumes the old
  "do not peek" answer at roughly the same cost.
- **`retroarch --scan` for the game's name** — recommended **not now**, and
  recorded as deliberately declined rather than missed. It is the only source
  for "this is Pokémon FireRed (USA)", but it costs a subprocess, a scratch
  `HOME`, a patched copy of the skeleton config to work around Ubuntu's empty
  `content_database_path`, and it returns nothing for the homebrew and ROM
  hacks a household most plausibly owns. Revisit if prefilling an entry's
  label from the ROM turns out to be what people want; the join key (`db_name`)
  is the same one the ladder already uses, so it drops in later without
  rework.

What must not happen either way: a file the ladder cannot identify being
treated as a bad value. No match narrows nothing and blocks nothing.

### 4. Where the installable-core catalogue lives

Duplicate `RETROARCH_CORES` in Rust, derive install hints from `.info` alone
(which does not know apt package names), or move the catalogue to one data file
that both `admin.sh` and the daemon read? Recommendation: **one data file**, or
failing that, no install hint at all — a wrong install command is worse than
none.

### 5. VDF parsing: hand-rolled or a crate?

Recommendation: **hand-rolled**, ~80 lines with fixtures. The format is
`"key" "value"` nesting; the crates for it are thin and unmaintained, and this
is a leaf parser with no protocol obligations.

### 6. Which fields get a picker?

The four the issue names, or every path-shaped field in the schema
(`process.command`, `process.cwd`, entry `icon`, `service.data_dir`,
`log_dir`, `child_log_dir`, `socket_path`, the TLS `cert`/`key`)?
Recommendation: **build one `<PathField>` and use it everywhere**, since after
the component exists each additional field is a prop.

### 7. Freshness

Fetch once per editor open, or offer an explicit refresh? Recommendation:
**fetch on open, refresh on picker open** — a parent who just installed a game
will reopen the picker, not the page.

### 8. How far to lean on the Steam client

Three positions, and the middle one is recommended:

- **Manifests only.** Simplest, offline, fully fixture-testable, and enough for
  an autocomplete. Costs the app-type filtering, which then has to be a name
  and appid heuristic that will misfile something.
- **Manifests, enriched by the client when it is already up** — recommended.
  The CDP primitive exists, `JSON.stringify` fits its string-valued
  `Runtime.evaluate` without touching the transport, and the only new plumbing
  is `title`/`url` on `CefTarget`. The enrichment is best-effort by
  construction: unreachable client, changed JS API, or a slow target all land
  on the manifest list.
- **Client only.** Rejected: it is unreachable in precisely the bootstrapping
  case — no Steam entry yet, so no `preload_steam`, so no running client and
  possibly no debug flag — that the feature is for.

And the sub-decision that is not a trade-off: **the picker may use the CEF
endpoint, never open it.** No writing `.cef-enable-remote-debugging`, no
restarting Steam, no preloading a client the policy did not ask for. If the
client is not there, the list is the manifests' and the field is still a text
field.

## Build order

0. **On a device with a logged-in Steam**: confirm the manifest layout, and
   evaluate the app-store query by hand against the running client's CEF
   endpoint. An hour that decides whether step 3 has one source or two, done
   before anything is built on the assumption.
1. `GET /api/v1/retroarch/cores` — installed scan + `.info` catalogue + the
   identification ladder, all in Rust with unit tests. No new security surface,
   and fully verifiable on this box.
2. The `DeviceFacts` seam and its `null` default, plus the core autocomplete.
   This is the smallest end-to-end slice and it proves the seam.
3. `GET /api/v1/steam/apps` — manifests first, the client enrichment second and
   separately, so the fallback is the thing that shipped rather than the thing
   that was meant to — plus the app-id autocomplete.
4. `GET /api/v1/files` + `<PathField>` + the picker dialog, wired to
   `content`, `book`, `library`, `core_path` — the biggest piece and the one
   carrying decision 2.
5. The remaining path fields (decision 6).
6. Docs: `KindEditor`'s helper texts stop being the only guidance;
   `docs/INSTALL.md` and `config.example.toml` can point at the picker instead
   of at hand-typed paths; this file gets its implementation record.
