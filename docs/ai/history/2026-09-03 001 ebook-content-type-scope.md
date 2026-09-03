# "ebook" content type — scope (issue #160)

> Issue: <https://git.armeafamily.com/albert/shepherd-launcher/issues/160>

## Prompt

> scope out #160

then, after the first pass landed on calibre:

> are there other existing epub and PDF packages that have better confinement

then:

> install okular and run the phase 0 checks

then:

> do it through phase 2

Scoping, a Phase 0 verification pass on hardware, and then the implementation:
the polite close, the `ebook` kind, and the docs. **This document is the design
record; what shipped is described in "What was built" at the end.**

## Issue text

> This is purely for offline content -- epubs, PDFs, and similar. Anything on a
> store and/or has DRM should be handled by that app -- i.e. Kindle and Google
> Play Books should be opened via the website via a browser activity or an
> Android activity (#2).
>
> The UX of this is primarily for novels and similar -- one activity per book
> that opens directly to the last opened page. No library for now.
>
> It may be sufficient to call Calibre specially for this to get this UX. In
> that case, all that's needed is an example activity.

## Summary

1. **calibre's viewer is the wrong reader for a supervised device.** It is not a
   kiosk and cannot be made into one: a tap on the page opens a menu with a file
   dialog, calibre's *Edit book* application, devtools and a Google web view.
2. **Okular can be, and it covers both halves of the issue.** KDE's Kiosk
   framework disables named actions from a config file the app cannot rewrite —
   the same class of documented control the browser kind uses with Chrome's
   enterprise policy. **Verified on hardware:** with the restrictions in place,
   Ctrl+O and Ctrl+P do nothing; without them, both open dialogs.
3. **But resume does not survive shepherd's graceful stop.** Okular installs no
   `SIGTERM` handler: the signal kills it outright and it writes no reading
   position at all. Asking the *compositor* to close the window instead
   (`xdg_toplevel.close`) makes it exit cleanly in well under a second, write
   its position, and resume there on the next launch. All three verified.
4. So the issue's "no code required" reading does not survive contact — for a
   second time, and for a different reason than RetroArch's in #125. The
   minimum honest deliverable is a **polite-close step in the graceful-stop
   path** plus the recipe; this is a general fix, not an ebook one.

## Why calibre falls over

calibre gets the reading experience right. Its standalone viewer restores a
per-book position keyed by the book's path, and writes that position on a 3 s
single-shot timer after every page turn (`gui2/viewer/ui.py`, `save_pos_timer`,
`setInterval(3000)`) — so a `SIGKILL` costs at most three seconds of reading. It
also handles `SIGTERM` (`setup_unix_signals` → `shutdown_signal_received` →
`request_close`, `gui2/__init__.py:1364-1387`), which suits shepherd's
single-SIGTERM stop. (Read from source; not exercised here — calibre was not
installed for these checks.)

What it will not do is stay out of the rest of the machine. The in-book overlay
— reached by *tapping the top third of the page*, no keyboard needed
(`pyj/read_book/view.pyj:120`) — offers, in the standalone viewer specifically
(`pyj/read_book/overlay.pyj`, the `runtime.is_standalone_viewer` branches):

| Overlay action | What the child gets | Source |
| --- | --- | --- |
| **Open book** | A file dialog over the whole filesystem — and a file dialog is also a place to rename and delete | `overlay.pyj:328`, `ui.py:576` |
| **Edit book** | calibre's *Edit book* application: code editor + file browser, for any EPUB/AZW3/KEPUB | `overlay.pyj:334` |
| **Print** | Print-to-PDF, i.e. another file dialog with a write path | `overlay.pyj:379` |
| **Inspector** | QtWebEngine devtools | `overlay.pyj:431` |
| **Lookup** | A live web view; the stock sources include `https://www.google.com/search?q={word}` — a general Google search, not a dictionary | `viewer/lookup.py:74-88` |
| **Preferences** | Every viewer setting, including the lookup source list | `overlay.pyj:361` |

An external link inside a book also escapes: `acceptNavigationRequest` hands it
to `safe_open_url` → the system browser, spawned outside anything shepherd
tracks (`viewer/web_view.py:412-423`).

None of it is switchable off. The toolbar's *contents* are a pref
(`vprefs['actions-toolbar-actions']`, `toolbars.py:129`) and the toolbar is
hidden by default (`toolbars.py:482`) — but the overlay is not the toolbar.
Patching calibre is out: the browser kind's own doc comment sets the rule —
shepherd "only wraps Chrome through documented controls — it does not patch the
browser" (`shepherd-config/src/schema.rs:265`).

## The survey: what else 26.04 ships

Versions are the 26.04 (`resolute`) candidates.

| Reader | Version | Formats it covers here | Resume | Confinement lever |
| --- | --- | --- | --- | --- |
| **Okular** + `okular-extra-backends` | 25.12.3 (universe) | PDF, **EPUB**, CBZ, FictionBook, DjVu, PS, DVI, XPS, markdown | Per-document viewport in `$XDG_DATA_HOME/okular/docdata`, restored on open — **but only written on a clean close** | **KDE Kiosk action restrictions** |
| **zathura** + poppler/cb/djvu | 2026.02.09 (universe) | PDF, CBZ, DjVu, PS — **no EPUB** (no `zathura-pdf-mupdf` in the archive) | Automatic (`open-first-page` defaults false) | No menus, toolbar or context menu at all; every escape is a key `unmap` removes; `--config-dir`/`--data-dir` per entry |
| **mupdf-gl** | 1.27.0 (universe) | PDF, **EPUB**, XPS, CBZ | `$MUPDF_HISTORY` / `$XDG_CACHE_HOME/.mupdf.history` | No open dialog at all when given a file (`gl-main.c:3435-3446`); link handler is `$BROWSER` (`gl-main.c:85`) |
| calibre `ebook-viewer` | 9.2.1 (universe) | EPUB/AZW3/MOBI/FB2/CBZ…, PDF only by lossy conversion | Automatic, 3 s debounce | **None** |
| Papers | 50.2 (main, installed) | PDF, comics, DjVu, TIFF — no EPUB | GFileInfo metadata, needs `gvfsd-metadata` | None |
| Foliate | 3.3.0 (universe) | EPUB, MOBI, FB2, CBZ | Yes | None; the library *is* the home screen |

### How the Okular lockdown works

KDE's Kiosk framework is enforced in the framework, not per app.
`KActionCollection::addAction` asks `KAuthorized::authorizeAction(name)` and, on
a "no", **disables the action, hides it, and blocks its signals**
(`kxmlgui/src/kactioncollection.cpp:349-354`) — menu item, toolbar button and
keyboard shortcut die together. Restrictions come from a `[KDE Action
Restrictions]` group in `kdeglobals` (`kconfig/src/core/kauthorized.cpp:194-228`),
and the `[$i]` immutability marker makes that group unwritable by the app
(`kconfigini.cpp:125-127`). No environment variable lifts it: `kde_kiosk_exception`
is an in-process global, default `false` (`kconfigini.cpp:18`).

Okular routes everything worth restricting through named actions
(`shell/shell.cpp:561-578`, `part/part.cpp:648-963`), so this is the block that
belongs in the entry's `$XDG_CONFIG_HOME/kdeglobals`:

```ini
[KDE Action Restrictions][$i]
action/file_open=false               # Ctrl+O and the menu item
action/file_open_recent=false
action/file_save_as=false
action/file_export_as=false
action/file_print=false
action/file_share=false              # Purpose plugins: "send to…"
action/open_containing_folder=false  # spawns the file manager
action/embedded_files=false          # extract files embedded in a PDF
action/import_ps=false
action/options_configure=false
action/options_configure_keybinding=false
action/options_configure_toolbars=false
action/options_show_menubar=false
action/help_report_bug=false
action/help_contents=false
shell_access=false
```

Two limits, both checked rather than assumed:

- **Okular never calls `KAuthorized` itself** — coverage comes entirely from
  `KActionCollection`. `[KDE URL Restrictions]` therefore does not bound what
  Okular will open; it constrains KIO callers only. Drag-and-drop onto the
  window is the remaining theoretical hole, and it is moot on a kiosk: there is
  no second app to drag from.
- Okular's **EPUB backend is a `TextDocumentGenerator`**
  (`generators/epub/generator_epub.cpp:18`) — Qt's rich-text engine, not a
  browser engine. No scripting, no remote fetches, nothing like calibre's
  embedded Chromium. See the rendering verdict below.

## Phase 0 results (run 2026-09-03, headless dev session, 1280x720, pixman)

Setup: `apt install okular okular-extra-backends`; two Project Gutenberg EPUBs
and a local PDF under `~/Books`; a three-entry fixture
(`dev-runtime/ebook-fixture.toml`) with a locked entry, an unrestricted control,
and a PDF entry, each with its own `XDG_CONFIG_HOME` / `XDG_DATA_HOME` /
`XDG_CACHE_HOME`; driven through `scripts/shepherd dev headless`, launches over
the daemon socket, keys via `wtype`.

### 1. The Kiosk restrictions bite — confirmed

| Key | Locked entry | Unrestricted control |
| --- | --- | --- |
| `Ctrl+O` | nothing happens | full file dialog, sidebar offering "Computer" and `$HOME` |
| `Ctrl+P` | nothing happens | print dialog, with a network printer and an "Output file" path picker |
| `Ctrl+Shift+S` | nothing happens | (not separately captured; save-as is restricted by the same key) |

The control matters: the same injected keys in the same session open the dialogs
without the `kdeglobals` block, so "nothing happened" is the restriction, not a
lost keypress.

### 2. Per-entry config isolation works

Each entry's `XDG_CONFIG_HOME` received its own `okularrc` / `okularpartrc`, and
its `XDG_DATA_HOME` its own `okular/docdata/<size>.<name>.xml`. Nothing was
written to the invoking user's `~/.config` or `~/.local/share`.

### 3. Native Wayland, and correct on HiDPI

`dev tree` reports `app_id: "org.kde.okular"` with `class: null` — a native
Wayland client, so sway rules can match the app id.

HiDPI matters more here than for any other activity type: reading is the one
thing where blurry text is the whole product. Re-run at 2560x1440 with the
output scaled, checked against the client's own protocol traffic
(`WAYLAND_DEBUG=1`) rather than by eyeballing:

| Output scale | What Okular received | What it did | Result at 1:1 |
| --- | --- | --- | --- |
| 2.0 | `wp_fractional_scale_v1.preferred_scale(240)`, `wl_surface.preferred_buffer_scale(2)` | `wp_viewport.set_destination(1280, 666)` | body text and UI chrome crisp |
| 1.5 (fractional) | `preferred_scale(180)` | `set_destination(1706, 906)` | crisp |

So Qt 6 binds `wp_viewporter` + `wp_fractional_scale_v1`, submits a buffer at the
device resolution and maps it to the logical size — the document raster, not just
the widget chrome, is drawn at full device resolution. Integer *and* fractional
scales are handled, and the scale change was picked up **live**, mid-session,
without a relaunch (relevant to the external-monitor/docking path, which changes
output scale under a running activity).

`xwayland_native_resolution` is therefore not needed and should not be set on
these entries.

One consequence worth knowing: with Fit Width the page always spans the full
device width, so the physical text size is the same at any scale — what a HiDPI
panel buys is sharpness, not larger text. The size knob is below.

### 3b. Reading text size is an admin setting, and it must be set before the child starts

Okular's EPUB backend takes its default font from a `KConfigSkeleton` named
`okular_epub_generator_settings` (`generators/epub/generator_epub.cpp:18`,
`core/textdocumentsettings.cpp:63`), which lands in the entry's config dir as a
file of that name, with the key in the *unnamed* group:

```ini
# $XDG_CONFIG_HOME/okular_epub_generator_settings
[No Group]
Font=Noto Serif,16,-1,5,400,0,0,0,0,0,0,0,0,0,0,1
```

Verified: it changes both the face and the size of the body text. `[General]`
does **not** work — the key has to be in `[No Group]`, which is where a
`KConfigSkeleton` with no `setCurrentGroup` puts it.

Two caveats. The font size changes **pagination** (Alice went from 63 to 67
pages at 22 pt), so a saved reading position shifts if the font is changed
later — pick it once, before the book is first opened. And with
`action/options_configure=false` in the restrictions, the child cannot change it
back, which is the point.

### 4. **shepherd's graceful stop loses the reading position**

This is the finding that changes the plan.

- Standalone: `okular -p 7 book.epub`, then a single `SIGTERM` → process gone,
  **`docdata/` empty**. Okular installs no `SIGTERM` handler; Qt's default
  disposition terminates the process before anything is saved.
- Through the kiosk: launch, page to 4, `stop_current` (which is shepherd's
  single-SIGTERM graceful path) → **no docdata written**.
- Positive control: a clean quit (`Ctrl+Q`) writes
  `docdata/188960.alice.epub.xml` with `<current viewport="8;…"/>`.
- Nothing is written *during* the session — no periodic flush, unlike calibre's
  3 s timer. It is all-or-nothing at close.

So today an Okular activity would resume from page 1 every time, and neither the
HUD's "end session" nor a time-limit expiry would ever save.

### 5. Asking the compositor to close the window fixes it — completely

`swaymsg '[app_id="org.kde.okular"] kill'` sends `xdg_toplevel.close`, which is
a *request*: Qt runs `closeEvent`, Okular saves, and the process exits.

- EPUB, standalone: close request → exit in ~2 s, docdata written, relaunch with
  no `-p` flag reopened at **page 9**, the page it was on.
- PDF, through the kiosk: paged to 3, close request → exit in **0.33 s**,
  docdata written, `launch` again → reopened at **page 3**.
- shepherd noticed the self-exit and returned to the launcher on its own; no
  orphan, no escaped window.

**shepherd already has every piece to do this.** `WindowInfo.id`
(`shepherd-api/src/types.rs:1224`) is the sway container id, window attribution
already tells the host which windows belong to the running activity, and
`sway::run_command` (`shepherd-host-linux/src/sway.rs:146`) sends arbitrary sway
commands over the IPC socket the daemon already holds open. A polite-close step
is `[con_id=N] kill`, a short wait, then the existing SIGTERM ladder.

This is **not** an ebook problem. Any GUI activity that saves on window close
but ignores `SIGTERM` loses state at end-of-session today — the same shape as
the double-SIGTERM bug #125 found, one layer up. It deserves its own issue.

### 6. Chrome can be mostly removed from config

Seeded into the entry's config dir, with `[$i]` where it takes:

```ini
# okularrc
[MainWindow][$i]
MenuBar=Disabled          # verified: menubar gone
[General][$i]
ShowSidebar=false         # verified: sidebar tab strip gone
LockSidebar=true

# okularpartrc
[Main View][$i]
ShowLeftPanel=false       # verified: contents panel gone
```

Two first-run popups also want suppressing: a "Welcome" bubble, and
presentation mode's "there are two ways of exiting presentation mode" modal.
Both are `KMessageBox` don't-show-again entries, so both are one seeded key.

### 6b. Getting to "only the book" — solved, in two parts

The toolbar is not a config setting at all. `KToolBar::applySettings` handles
only icon size and button style (`kxmlgui/src/ktoolbar.cpp:1094`); visibility
lives in the **XMLGUI `.rc` file's `hidden` attribute** (`ktoolbar.cpp:1035`,
written back at `:1067`), which is why every `[MainWindow][Toolbar mainToolBar]`
variant was ignored. The local override lives at
`$XDG_DATA_HOME/kxmlgui5/<component>/<file>.rc` (`kxmlguiclient.cpp:160`), which
the per-entry `XDG_DATA_HOME` already isolates.

Seeding okular 25.12.3's own `part.rc` (component `okular_part`, version 55) and
`shell.rc` (component `okular`, version 11) with `hidden="true"` on the
`mainToolBar` element **hides the toolbar**, and okular leaves the seeded files
untouched across a session (it only rewrites them when the user edits toolbars,
which `action/options_configure_toolbars=false` forbids). The catch is version
coupling: KXMLGUI merges local against built-in by `version=`, so an okular
upgrade that bumps it silently drops the customisation. Whatever ships this must
re-derive the file per installed version, or at least warn when the versions
diverge.

Combined with the page-at-a-time settings, this is the full config set — all of
it verified on screen:

```ini
# okularrc
[MainWindow][$i]
MenuBar=Disabled
[General][$i]
ShowSidebar=false
LockSidebar=true

# okularpartrc
[Main View][$i]
ShowLeftPanel=false
[PageView][$i]
ShowScrollBars=false
UseCustomBackgroundColor=true
BackgroundColor=#ffffff        # the letterbox around the page, not the paper
ViewContinuous=false           # page at a time, no scrolling between pages
ViewMode=Facing                # Single | Facing | FacingFirstCentered
TrimMode=None                  # Margins trims the page's white border
[Zoom][$i]
ZoomMode=2                     # 0 fixed 100%, 1 Fit Width, 2 Fit Page, 3 Fit Auto
[General][$i]
ShowOSD=false

# okular_epub_generator_settings
[No Group]
Font=Noto Serif,16,-1,5,400,0,0,0,0,0,0,0,0,0,0,1
```

What is left on screen is the book and shepherd's own HUD: no menubar, no
toolbar, no sidebar, no scrollbars, no page-count overlay. The only furniture
okular still draws is a hairline frame around each page, which has no setting and
reads as a page edge anyway.

**`ViewMode=Facing` is the answer on a landscape panel.** One portrait page fitted
to a 16:9 screen is letterboxed and small; two facing pages fill the width, and
the result reads like an open book at a comfortable size. On a portrait tablet,
`Single` is the one to use. `FacingFirstCentered` puts the cover on its own,
which is the more book-like opening.

**Text size is set by the EPUB font, not by zoom.** In `ZoomMode=2` the page is
scaled to the screen, so a larger `Font` means fewer words per page and larger
apparent text. That is the one knob to tune per device, and it repaginates (see
3b), so set it before the child starts the book.

### 6d. How much config this actually is

Measured on the working setup, and split by what has to be repeated per book:

| File | Lines | Repeated per book? |
| --- | --- | --- |
| `config/kdeglobals` (the Kiosk restrictions) | 17 | **No** |
| `config/okularrc` (menubar, sidebar) | 15 | **No** |
| `config/okularpartrc` (page-at-a-time, chrome, background) | 16 | **No** |
| `config/okular_epub_generator_settings` (reading font) | 2 | **No** |
| `data/kxmlgui5/okular_part/part.rc` (toolbar hidden) | 150 | **No** — okular's own file, one attribute changed |
| `data/kxmlgui5/okular/shell.rc` (toolbar hidden) | 30 | **No** — same |
| The `[[entries]]` block | 13 | Yes |

**The 50 lines of hand-written INI and the two derived `.rc` files are once per
device, not once per book.** Verified: a single shared `XDG_CONFIG_HOME` /
`XDG_DATA_HOME` across several books keeps *independent* reading positions,
because okular names docdata `<size>.<filename>.xml` — opening Alice, then the
PDF, then Alice again resumed Alice exactly where it was left. The recent-files
list is the only shared state, and `action/file_open_recent=false` makes it
unreachable.

So the marginal cost of the second book is a 13-line TOML block, of which 3 lines
are `XDG_*` paths that only exist because `[entries.kind.env]` does not expand
`~`, and 3 more are the `process`/`command`/`args` boilerplate that an `ebook`
kind would collapse into `book = `.

Which puts numbers on the Phase 1 / Phase 2 split:

| | First book | Each subsequent book |
| --- | --- | --- |
| Today, by hand | ~200 lines across 6 files + 13 TOML | 13 TOML |
| Phase 1 (`shepherd-admin apps install okular` writes the shared files) | 13 TOML | 13 TOML |
| Phase 2 (`ebook` kind owns config generation) | ~6 TOML | ~6 TOML |

The honest read: Phase 1 removes the part that is genuinely unreasonable to ask
of an admin (a 150-line XMLGUI file and a Kiosk block nobody can be expected to
know about). Phase 2 is a nicety on top — worth doing when the shelf grows, not
before.

### 6c. The fullscreen alternative, and why not to use it

`swaymsg '[app_id="org.kde.okular"] fullscreen enable'` also produces a
chrome-free window: okular's own fullscreen handler hides the menubar and toolbar
(`shell/shell.cpp:795-810`), and — usefully — **the HUD stays visible**, because
it is a layer-shell surface on `Layer::Overlay` (`shepherd-hud/src/app.rs:199`),
which sway renders above fullscreen windows.

But a fullscreen window ignores the HUD's exclusive zone, so the top ~80 px of
the page sits behind the HUD and the first lines of every page are unreadable.
The local-`.rc` route above keeps the exclusive zone and loses nothing, so it is
the one to take. Worth knowing the overlay-layer behaviour exists, though: it
means shepherd *could* let an activity go fullscreen without losing the HUD, if
some future activity type wants that.

### 7. Presentation mode is not the reading mode

`--presentation` does produce a chrome-free page — and sway's fullscreen denial
does not break it; the window simply fills the area below the HUD. But for a
reflowed EPUB it fits an entire source "page" to the window height, which
rendered Alice's chapter text at roughly 9 px. Unreadable.

Fit Width in the ordinary scrolling window was properly legible at 720p — body
text at a comfortable size, italics preserved, paragraph indents intact, TOC
parsed with page numbers, cover image in colour. Okular's EPUB backend is plainer
than calibre's Chromium renderer, but for a text novel it is entirely good
enough; this was the check that could have sent us back to calibre, and it did
not. (Result 6b then improves on Fit Width: page-at-a-time facing pages read
better and are what the issue asks for.)

PDF rendering was, as expected, exactly right, with its outline in the sidebar.

### 8. Timings

Launch (over the daemon socket) to mapped window: **~5 s**, measured on the
146 KB PDF entry, on a debug build with no GPU. Okular keeps no converted-book
cache the way calibre does, so there is no first-run penalty to amortise — but
nor is there a shortcut on later opens. Close request to exit: **0.3 s** (PDF)
to **~2 s** (EPUB).

### 9. Harness notes (worth folding into the `headless-dev` skill)

- Qt/KDE clients take injected keys, but only with the repeat trick the skill
  documents for winit: `wtype -s 800 -M ctrl -k o -m ctrl` twice or three times;
  a single press is lost to the keyboard-capability race.
- Menubar mnemonics (`Alt+F`) never landed, and `dev click` on the menubar did
  nothing — same synthetic-pointer limitation the skill records for GTK. Verify
  menu *contents* by their keyboard shortcuts instead.
- Do not `pkill -f "okular …"` from a `bash -c` driver: the pattern matches the
  driver's own command line and kills the script (exit 144). `pgrep -x okular`
  and kill the pids.

## Design decisions

| Question | Decision |
| --- | --- |
| Which reader does shepherd recommend? | **Okular**, with `okular-extra-backends` for EPUB. One app for both halves of the issue, the only lockdown that is a documented control, and its EPUB rendering passed the readability check. |
| Where does calibre end up? | Documented as the alternative for typography-heavy EPUBs, with its escape surface stated plainly. Not the example. |
| Is code required after all? | **Yes, but not ebook code.** The graceful stop must ask the compositor to close the window before signalling, or no Okular activity ever remembers a page. Small, general, and useful to every GUI activity. |
| Where does that live? | `shepherd-host-linux`: in the graceful-stop path, `[con_id=N] kill` for the activity's windows via the existing `sway::run_command`, wait for exit up to a short budget, then the current SIGTERM → SIGKILL ladder unchanged. Behind a per-kind or per-entry flag if it turns out to upset anything that treats a close request as "minimise". |
| Is a new entry *kind* required? | Still no, but the case is stronger than it was: the kind would own the `kdeglobals` generation, the config-dir wiring, and the diagnostics. Phase 1 can ship without it. |
| Kind name, if we build one | **`ebook`**, with a `viewer = "okular" \| "calibre" \| "zathura" \| "mupdf"` arm. |
| How settings reach the reader | Per-entry `XDG_CONFIG_HOME` / `XDG_DATA_HOME` / `XDG_CACHE_HOME`, with shepherd re-rendering `kdeglobals`, `okularrc` and `okularpartrc` before every spawn. `[$i]` makes the restrictions immutable within a session; re-rendering makes them immutable across sessions. |
| Position storage | The reader's own `okular/docdata`. shepherd never writes positions by hand, and re-rendering config must not touch that directory. |
| Confinement beyond the app | `[entries.firewall] default = "deny"` on every reading entry, and #105 as the eventual answer to any dialog that survives. |
| Reading mode | Page at a time (`ViewContinuous=false`) at Fit Page, `ViewMode=Facing` on a landscape panel and `Single` on a portrait one. **Not** `--presentation`, and not the Fit Width scroll view. |
| Chrome | All of it off: menubar, sidebar, scrollbars and page-count overlay from config, toolbar from a seeded local XMLGUI `.rc`. See Phase 0 result 6b for the exact set. |
| HiDPI | Nothing to do: Okular is a native Wayland client that honours integer and fractional output scale, and renders the page at device resolution. Do **not** set `xwayland_native_resolution`. Set the reading size with the EPUB backend's `Font` key instead. |
| Icon | Explicit `icon = ` per entry, as with RetroArch box art. |

## Phase 1 — the recipe (1 day, after the close fix)

Deliverables: a `config.example.toml` block, `docs/ebooks.md`, and a
`shepherd-admin apps install okular` arm beside the RetroArch one
(`scripts/lib/admin.sh:790+`). The admin arm is what writes the `kdeglobals`
restrictions and the chrome settings into the entry's config dir — by hand it is
a wall of INI nobody will type twice.

```toml
[[entries]]
id = "the-hobbit"
label = "The Hobbit"
icon = "~/Books/covers/the-hobbit.png"
group = "reading"

[entries.kind]
type = "process"
command = "okular"
# `args` are tilde-expanded at spawn; `env` values are not (see below).
args = ["~/Books/the-hobbit.epub"]

# Settings, restrictions and reading positions live here instead of in the
# admin's own KDE config. `shepherd-admin apps install okular` seeds them.
[entries.kind.env]
XDG_CONFIG_HOME = "/home/kid/.local/state/shepherd/ebook/the-hobbit/config"
XDG_DATA_HOME = "/home/kid/.local/state/shepherd/ebook/the-hobbit/data"
XDG_CACHE_HOME = "/home/kid/.local/state/shepherd/ebook/the-hobbit/cache"

# A book needs no network. Belt to the Kiosk braces.
[entries.firewall]
default = "deny"

# D-pad = arrow keys = page turn, for a gamepad-only device.
input_compat = "gamepad_productivity"
```

Note the absolute paths: `args`, `command` and `cwd` are tilde-expanded at spawn
(`adapter.rs:1702-1707`), but `env` is passed through as `env.clone()`, so a
`~/…` would reach the app literally. Every recipe therefore carries a hard-coded
home directory — exactly the paper cut a kind would remove.

`docs/ebooks.md` covers: which reader for which format, the Kiosk block and what
each line closes, that a reading position is only saved when the window is
closed properly (and what shepherd does about that), where positions live and
how to back them up, the DRM boundary (Kindle and Play Books are browser or
Android activities, per the issue), the calibre and zathura alternatives with
their surfaces stated, and the token recipe.

Worth putting in the example, because it is why a parent wants this activity type
at all: reading as a **token source** (#8) — `[entries.tokens] from =
["the-hobbit"]` on a game entry banks game time for time spent reading, with no
limit on the reading entry itself.

## Phase 2 — the `ebook` kind (2-3 days, only if Phase 1 chafes)

```toml
[entries.kind]
type = "ebook"
book = "~/Books/the-hobbit.epub"     # absolute or ~/-prefixed, as retroarch's `content`
# viewer = "okular"                  # okular (default) | calibre | zathura | mupdf
# open_at = 1                        # first launch only; ignored once a position exists
```

`crates/shepherd-host-linux/src/ebook.rs`, modelled on `retroarch.rs` but
smaller — no control socket, no reset button, no save-state lifecycle:

- Per-entry state root `<state>/shepherd/ebook/<entry-id>/{config,data,cache}`,
  keyed by `SpawnOptions::entry_id`, exported as the three XDG variables (or the
  reader's own flags where it has them — zathura's `--config-dir`/`--data-dir`,
  calibre's `CALIBRE_CONFIG_DIRECTORY`, mupdf's `MUPDF_HISTORY`).
- Re-render the restriction and chrome config on **every** spawn. Never touch
  `data/okular/docdata/`.
- argv: `okular [-p <page>] <book>`, `command` used as given and tilde-expanded,
  exactly as `retroarch.rs` treats its own (`retroarch.rs:761`).
- Diagnostics beside the RetroArch ones (`shepherdd/src/diagnostics.rs`): reader
  binary missing, EPUB backend missing (`okular-extra-backends` absent is the
  likely first-run failure), book missing or unreadable, extension the chosen
  reader does not handle.
- A `warn` when the generated config has been edited by hand, the same idea as
  `retroarch::conflicting_overrides`.

Boilerplate any new kind costs, from #129's diff: `shepherd-api` (`EntryKind`,
`EntryKindTag`) · `shepherd-config` (`schema.rs`, `policy.rs`, `validation.rs`,
`icon.rs`, `bin/validate-config.rs`) · `shepherd-host-linux` (spawn arm,
capabilities, the new module) · `cargo run -p shepherd-wire-codegen --bin
rpc-codegen` regenerating `docs/rpc-schema.json`, the webui TS and the companion
Kotlin · `shepherd-webui/src/config/components/KindEditor.tsx` (exhaustive match)
· `config.example.toml` · crate READMEs · an e2e test alongside
`crates/shepherd-e2e/tests/retroarch.rs`.

## Option B — calibre-server behind the browser kind

The only way to have calibre's rendering *and* real lockdown. calibre's content
server serves the same reader as a web app, and the web build takes the **other**
branch of every conditional in the escape table: no Open book, no Edit book, no
Inspector, no Print (`overlay.pyj:315-336`, the `else` arms). Run
`calibre-server --listen-on 127.0.0.1 --enable-auth` as a user service and point
a `[entries.browser]` entry at the book's reader route with `url_allowlist`
pinned to that origin.

Costs: it needs a calibre *library* (`calibredb add`), which the issue defers;
the reader route is a fragment-encoded SPA path
(`#book_id=…&fmt=EPUB&mode=read_book`, `pyj/book_list/router.pyj:102-115`), and
Chrome's URL policy cannot constrain in-page navigation, so the child can still
reach the library page. Keep it documented; do not build toward it.

## Out of scope / follow-ups

- **Polite close before SIGTERM** — the finding above, worth its own issue: it
  affects every GUI activity that saves on window close, not just readers.
- **A library mode** — deferred by the issue; if it happens, the shape is
  shepherd-media's, not a reader's.
- **Cover art as the tile icon** — wants a place to run `ebook-meta --get-cover`
  that is not config parsing. Own issue.
- **#105 per-activity filesystems** remains the general answer to any file
  dialog Kiosk does not cover; **#106** touches the same surface.
- **#28 configuration recipes** would generate one entry per book, which is what
  makes "one activity per book" scale past a shelf.
- Hiding Okular's main toolbar from config (see Phase 0 result 6).

## Open questions for the maintainer

1. **Is the polite-close step acceptable as a general change to graceful stop?**
   It is small and it is the only thing standing between an Okular activity and
   a remembered page. The alternative — per-kind special-casing — is worse, and
   the same fix silently helps every other GUI app that ignores `SIGTERM`.
2. **Phase 1 only, or Phase 1 then Phase 2?** The kind is ergonomics and
   diagnostics: one `book = ` line instead of a dozen, no hard-coded home
   directory, a diagnostic when the book or the EPUB backend is missing, and
   restrictions that cannot rot.
3. **PDFs through Okular, or a second reader?** Okular covering both is the
   simplicity argument; zathura is smaller and nicer on a keyboard-less device,
   at the cost of a second reader to document — and it needs the same
   polite-close treatment checked before it can be recommended.

## What was built

All of it, in one pass, after Phase 0 settled the open questions.

**The polite close** (`shepherd-host-linux/src/adapter.rs`). A graceful stop for
a kind that opts in (`EntryKind::wants_polite_close`) first asks the compositor
to close every window attributed to the session — `[con_id=N] kill` over the
sway IPC the daemon already holds — waits up to `POLITE_CLOSE_TIMEOUT` (3 s),
and only then falls through to the unchanged `SIGTERM` → `SIGKILL` ladder. The
wait only happens if a window was found, so an activity without one costs
nothing. Opt-in per kind for the reasons in the doc comment: Steam reads a close
request as "hide to tray", and RetroArch's single-`SIGTERM` shutdown is already
verified to save. `SessionInfo::retroarch` became `graceful_floor:
Option<Duration>` in passing, since two kinds now want a longer stop window for
the same reason.

Measured in the headless session afterwards: **108 ms** from the close request
to the reader exiting, with the reading position written. Under the old
signal-only path the same stop wrote nothing at all.

**The `ebook` kind** (`shepherd-host-linux/src/ebook.rs`, plus the usual spread
through api / config / capabilities / diagnostics / codegen / webui). It renders
the whole reader configuration per entry and re-renders it before every launch —
`kdeglobals` restrictions (immutable, 18 actions), `okularrc` / `okularpartrc`
for the view, the EPUB font, and the two local XMLGUI documents that hide the
toolbar — into a per-entry `XDG_CONFIG_HOME` / `XDG_DATA_HOME` / `XDG_CACHE_HOME`.
Reading positions live under that root and are never written by shepherd.

Config surface, which is the point of the kind:

```toml
[entries.kind]
type = "ebook"
book = "~/Books/the-hobbit.epub"
```

Everything else — `layout`, `font_size`, `font_family`, `open_at`, `kiosk`,
`viewer`, `command`, `args`, `env` — has a default that matches what Phase 0
found to work.

**Diagnostics.** `EbookBookMissing` for a book that is not there, and
`EbookReaderMissing` for a reader or a format backend that is not installed —
the second because on Ubuntu, EPUB support ships separately from Okular, which
is the likely first-run failure and one no log line reaches.

**Docs and packaging.** `docs/ebooks.md` (the operator's page, including what
this is *not*), a `config.example.toml` entry, `shepherd-admin apps install
okular` (which installs `okular-extra-backends` and the default font, because
that is the part people miss), README and crate READMEs.

**Tests.** Unit tests for the rendering and the path keying
(`ebook.rs`), for which kinds opt into the polite close and how the floors
relate (`adapter.rs`), and two e2e tests (`shepherd-e2e/tests/ebook.rs`): the
missing-book diagnostic reaching a client, and a launch actually materializing
the restrictions — a spawn path that skipped them would leave the activity wide
open with every unit test still green.

### Verified end to end

Against the real Okular, in the headless session, through the real kind:

- A six-line entry produces the chrome-free facing-pages view.
- `stop_current` → position written (108 ms), relaunch → the same spread.
- Two entries on the same book keep independent positions; the per-entry font
  and layout reach the reader (16 pt facing vs 20 pt single).
- Ctrl+O does nothing in a `kiosk = true` entry.
- A PDF works through the same kind.

### Touch: the gap the defaults had, and what was done about it

Asked after the fact — "how are the touch gestures here" — and the answer was
not good. From Okular's source (`part/pageview.cpp`): the desktop view grabs
exactly one gesture, **pinch**, which zooms. There is no swipe-to-turn. Touch
otherwise arrives as synthesized mouse events, and a drag feeds the kinetic
scroller, which pans. Turning a page is bound to keys and to the wheel at a
page's top or bottom edge (`wheelEvent`, which explicitly advances in
non-continuous mode) — and a touchscreen produces neither.

**So the shipped default (`facing`, fitted, paged) could not be turned at all on
a touch-only device.** A child would have been stuck on page one.

Two things went in for it:

- **`layout = "scroll"`** — one continuous column fitted to the width
  (`ViewContinuous=true`, `ZoomMode=1`), which a drag scrolls. Not the default,
  because it is the wrong shape everywhere else.
- **The `EbookNoPageTurn` diagnostic** — a paged entry on a device whose
  connected inputs are a touchscreen and nothing else. It reuses the input
  enumeration the `requires_input` gate already runs, names `layout = "scroll"`
  as the fix, and stays quiet when a keyboard or gamepad is attached or when
  `/dev/input` cannot be read.

**A correction worth recording.** Mid-investigation I drove `swaymsg seat -
cursor` drags at the reader and read "nothing moved" as a finding. It was not:
the headless seat reports **capabilities 0 with no devices**, so those events
reached no client at all. The same limit explains the earlier `dev click`
failures. `wtype` works only because it creates a *virtual keyboard*; there is
no equivalent for pointer or touch, and the libinput backend needs a real seat
(it fails with `Could not open terminal to clean up VT 0` over SSH). **Touch
cannot be exercised in this harness**, so everything in this section is
source-derived and wants confirming on a panel.

### Page-turn buttons in the HUD

Where the touch gap actually got closed, and the better answer than a gesture
sidecar: the HUD already sits on the overlay layer, above the activity, on every
screen — and unlike anything inside the reader, no reader restriction can take
it away. A reading session now adds `‹` and `›` to the bar; pressing one
synthesizes `Page Up` / `Page Down` through the same `/dev/uinput` backend the
input bridges use (`shepherd-bridge`, `UinputSink::new_keyboard`), which the
compositor delivers to whatever holds keyboard focus — the activity, because the
HUD's bar takes none.

Plumbed the way the reset button already was: `EntryKind::supports_page_turn()`
→ `SessionPlan` → the `SessionStarted` event → `SessionState::can_turn_pages()`
→ button visibility. Nothing new in the RPC surface.

**Verified in the harness**: pressing a button (through the debug trigger the
`headless-dev` skill documents, extended with `.page_next` / `.page_prev`)
creates the device and emits exactly `KEY_PAGEDOWN` / `KEY_PAGEUP`, read back
from the device node. The last hop — compositor reads uinput, delivers to the
reader — is the one the harness cannot show, because it runs with no input
devices at all. It is the same hop the touch and gamepad bridges already depend
on.

Two things the work turned up on the way:

- **The bar was already full.** Two more buttons pushed the end-session `X` off
  the end, where GTK clips rather than wraps. Fixed by ellipsizing the activity
  name (a latent bug: a long name could already clip the `X`), shortening the
  sliders and hiding the volume/brightness percentages while reading. The
  label's `width-chars` floor is 12 — measured, not guessed: at 18 the `X` fell
  off again, and a session a child cannot end is worse than a truncated title.
- **The `EbookNoPageTurn` diagnostic changed meaning.** It was "a touchscreen
  cannot turn this page"; the buttons make that false. It now fires only when
  the buttons *also* cannot work — touch-only device **and** `/dev/uinput` not
  writable — and names both fixes.

### Left undone, deliberately

- **The polite close is opt-in for one kind.** Extending it to plain `process`
  entries is a one-line change and a judgement about stop latency for apps that
  ignore the request; worth doing once there is a second app that wants it.
- **Cover art as the tile icon** still wants a place to run `ebook-meta
  --get-cover` that is not config parsing (#160 follow-up, own issue).
- **A second reader.** `viewer` exists as an enum with one value; zathura and
  mupdf are sketched in the survey above but nothing is wired.
- **A touch page-turn *gesture*.** The HUD buttons below solve the problem;
  tapping the page itself would still be nicer, and is the sidecar sketched
  earlier (tap zones → `Page Up`/`Page Down`, in the shape of
  `touch_to_mouse`). Not started.
