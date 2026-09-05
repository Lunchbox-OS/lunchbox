# Books

`shepherdd` runs reading activities through [Okular][okular], KDE's document
viewer, with `type = "ebook"` entries. One entry is one book, and opening it
puts the child back on the page they stopped on.

Compared with launching a reader as a plain `type = "process"` activity, the
dedicated kind exists for two reasons, and neither is cosmetic:

- **The child stays in the book.** Okular's defaults offer a file dialog over
  the whole filesystem, a print dialog with a write path, a settings dialog and
  a menubar. All of it is closed off, using KDE's own Kiosk framework rather
  than anything shepherd patches.
- **The page is remembered.** Okular writes its reading position only when its
  window closes cleanly, and it installs no `SIGTERM` handler at all — so a
  signal-only stop loses the place every time. shepherd asks the compositor to
  close the window first, and only then signals.

**No books are included, and none can be.** Supply your own, DRM-free. A book
from a store belongs in that store's app — Kindle Cloud Reader or Google Play
Books in a browser activity, or an Android activity — because the DRM is the
point of the store.

[okular]: https://okular.kde.org/

## Installing

```sh
sudo shepherd-admin apps install okular
```

That installs three packages, and the second is the one people miss:

| Package | Why |
| --- | --- |
| `okular` | The reader. Handles PDF, CBZ/CBR, FictionBook, PostScript, DVI, XPS. |
| `okular-extra-backends` | **EPUB**, DjVu, Markdown and TIFF. Packaged separately from Okular, so without it a reading activity opens PDFs and refuses novels. |
| `fonts-noto-core` | Provides `Noto Serif`, the default reading font. |

If the reader or a format's backend is missing, shepherd says so as an entry
diagnostic (`EbookReaderMissing`) rather than leaving the child to find out.

## Configuring an activity

```toml
[[entries]]
id = "the-hobbit"
label = "The Hobbit"
icon = "~/Books/covers/the-hobbit.png"

[entries.kind]
type = "ebook"
book = "~/Books/the-hobbit.epub"

[entries.availability]
always = true

[entries.firewall]
default = "deny"
```

### Fields

| Field | Default | Notes |
| --- | --- | --- |
| `book` | required | Absolute or `~/`-prefixed. A relative path would resolve against the daemon's working directory, not yours. |
| `layout` | `facing` | `facing` (two pages, like an open book), `facing_first_centered` (same, cover alone), `single` (one page), `scroll` (a continuous column — the only one a touch-only screen can navigate; see "Touch"). |
| `font_size` | `16` | Points, for a reflowed EPUB. See "Text size" below. |
| `font_family` | `Noto Serif` | Must be installed on the device. |
| `open_at` | — | 1-based page, **first launch only**; afterwards the remembered position wins. |
| `kiosk` | `true` | `false` unlocks the reader's own menus, file dialog and settings. For an admin looking at what Okular does unrestricted — not for a child. |
| `viewer` | `okular` | The only reader wired up. |
| `command` | the viewer's name | An absolute path, or a different binary. |
| `args`, `env` | — | Appended after what shepherd derives; `env` is layered over the generated `XDG_*` variables. |

### Which layout

A page is a page: the paged layouts fit the view to the screen and do not
scroll, so turning the page turns the page.

- **`facing` on a landscape panel.** One portrait page fitted to a 16:9 screen
  is letterboxed and small; two side by side fill the width and read like an
  open book.
- **`single` on a portrait screen**, where one page already fills it.
- **`scroll` on a touch-only screen** — see below. It is the one layout that
  can be navigated by touch alone.

### Touch

**A paged layout cannot be turned by touch.** Read the next paragraph before
setting up a tablet.

Okular's desktop view grabs exactly one gesture — pinch, which zooms
(`part/pageview.cpp`, `grabGesture(Qt::PinchGesture)`). There is no
swipe-to-turn anywhere in it. Everything else a finger does arrives as a
synthesized mouse event, and a drag feeds Okular's kinetic scroller, which pans
the view. Turning a page is bound to keys (`Page Down`, `Space`, arrows), to
the scroll wheel at the top or bottom of a page (`wheelEvent`), and to nothing
else. A touchscreen produces none of those.

**So shepherd puts the page buttons in the HUD.** A reading session adds a
`‹` and a `›` to the HUD bar, beside the reset and end-session buttons. They are
shepherd's own surface — on the overlay layer, above the activity, and outside
anything the reader's own restrictions could take away — and pressing one
synthesizes the `Page Up` / `Page Down` the reader is already listening for,
through the same `/dev/uinput` device the input-compat bridges use.

That makes every device navigable:

| Input | Paged layouts | `layout = "scroll"` |
| --- | --- | --- |
| Keyboard | Page Up/Down, arrows, Space | scrolls |
| Gamepad (`input_compat = "gamepad_productivity"`) | D-pad → arrow keys | scrolls |
| Mouse / touchpad | wheel turns the page | wheel scrolls |
| **Touchscreen only** | **the HUD's `‹` `›` buttons** | drag scrolls, or the same buttons |

`layout = "scroll"` remains the alternative for a touch-only device where
scrolling reads better than paging: one continuous column fitted to the width,
dragged like a phone. It is not the default because it is the wrong shape
everywhere else.

The buttons need `/dev/uinput` to be writable by the session user — the same
access the touch and gamepad bridges need. Where it is not, and the device has
no keyboard or gamepad either, shepherd raises the `EbookNoPageTurn` diagnostic
rather than leaving a child on page one; it names both fixes (the uinput access,
or `layout = "scroll"`).

While the page buttons are on the bar, the volume and brightness *percentages*
step aside and their sliders shorten: the bar is full at a typical panel width,
and something had to give way for two more controls. The sliders still show the
level, and the activity name still fits.

**Partly verified.** The HUD buttons were driven end to end in the headless
harness: pressing one creates the virtual keyboard and emits exactly
`KEY_PAGEDOWN` / `KEY_PAGEUP`, read back from the device node. What that harness
cannot show is the last hop — the compositor reading a uinput device and
delivering the key — because it runs with no input devices at all (its seat
reports no pointer, keyboard or touch capability, and the libinput backend needs
a real seat). That hop is the one the touch and gamepad bridges already rely on.
Everything about what a *finger* does in the reader is from Okular's source, not
a measurement, and is worth checking the first time a panel is set up.

### Text size

`font_size` is the reading-size knob, not zoom. Okular reflows an EPUB at that
size, paginates from it, and then fits the page to the screen — so a larger
font means fewer words per page and larger text on it.

It follows that changing `font_size` **repaginates the book**, and a remembered
position is a page number. Pick the size before the first read; changing it
later moves where the child left off.

A fixed-layout format (PDF, comics) ignores it entirely — the pages are already
laid out, and the reader only scales them.

## How the page is remembered

Okular keeps a per-document record in
`<state>/ebook/<entry-id>/data/okular/docdata/<size>.<name>.xml`, and restores
the viewport from it on the next open. shepherd never writes those files.

Two things follow.

**The position is written on a clean close, and only then.** There is no
periodic flush. This is why `stop` asks the compositor to close the window
(`xdg_toplevel.close`, a request the reader can act on) before it sends
`SIGTERM`, and why an `ebook` session gets a longer graceful-stop window. A
crash or a power cut still costs the current page.

**A book is identified by size and filename.** Replacing a book with a
different edition of the same name and size — an unlikely accident — inherits
the old position; renaming it, or re-downloading a different scan, starts over.

## Closing a book

The HUD's close button ends most activities behind an "are you sure" prompt,
because something unsaved may be lost. A book has nothing to lose: the page is
written on the way out, and reopening lands back on it. So an `ebook` entry
defaults to `confirm_on_close = false` — one tap and the book closes.

That is a default of the kind, not of the field. Setting `confirm_on_close`
explicitly on the entry always wins, in either direction:

```toml
[[entries]]
id = "hobbit"
confirm_on_close = true   # ask anyway
```

## Where files live

Everything the reader writes goes under the daemon's data directory, keyed by
entry id:

```
<data>/ebook/<entry-id>/
├── config/                            # XDG_CONFIG_HOME
│   ├── kdeglobals                     #   Kiosk action restrictions
│   ├── okularrc                       #   menubar, sidebar
│   ├── okularpartrc                   #   page view, layout, background
│   └── okular_epub_generator_settings #   reading font
├── data/                              # XDG_DATA_HOME
│   └── okular/docdata/                #   ← the reading positions
└── cache/                             # XDG_CACHE_HOME
```

Keyed by entry, so two entries pointing at the same book keep separate
progress, and backing up (or resetting) one child's reading is one directory.

The admin's own `~/.config` is never touched — which also means a child's
reading never appears in an admin's recent-files list, and an admin's Okular
settings are not something a child can reach.

## What shepherd generates, and what it leaves alone

Every file under `config/` is re-rendered **before each launch**. Okular rewrites its own configuration when it exits, so
a one-time seed would decay; re-rendering means the restrictions hold across
sessions as well as within one. Hand edits to those files are overwritten. The
`docdata/` directory is never touched.

Okular has no single "kiosk" switch, so three mechanisms are needed — and the
third is not the one the documentation would lead you to.

### 1. Action restrictions (`kdeglobals`)

KDE's Kiosk framework, applied by the framework rather than by the app:
`KActionCollection::addAction` asks `KAuthorized::authorizeAction`, and on a
"no" it disables the action, hides it and blocks its signals — so the menu
item, the toolbar button *and* the keyboard shortcut die together. The `[$i]`
marker makes the group immutable to the application, and no environment
variable lifts it.

What is closed: `file_open` (Ctrl+O, a filesystem browser), `file_open_recent`,
`file_save_as`, `file_export_as`, `file_print`, `file_print_preview`,
`file_share`, `open_containing_folder` (spawns the file manager),
`embedded_files` (extracts files embedded in a PDF), `import_ps`, the four
`options_configure*` dialogs, `options_show_menubar`, `options_show_toolbar`,
the help and bug-report items, plus the generic `shell_access` and
`movable_toolbars` — and the two the *toolbar* opens, `hamburger_menu` (the
menubar in one button) and `show_leftpanel` (the sidebar toggle).

### 2. The view (`okularrc`, `okularpartrc`)

Menubar off, sidebar off, scrollbars off, on-screen messages off; page at a
time (`ViewContinuous=false`) fitted to the screen (`ZoomMode=2`); the surround
painted white so a portrait page on a landscape screen is not framed in grey.

### 3. The toolbar, via full-screen mode

The toolbar takes the most explaining. Its visibility is not a config key —
`KToolBar` reads it from the `hidden` attribute of the XMLGUI definition — and
supplying that definition does not work either. Measured on a device, the
toolbar survives an empty config, `[MainWindow][Toolbar mainToolBar]
Hidden=true`, and a local XMLGUI document declaring `hidden="true"`, whether
minimal or a full copy of Okular's own with a version stamp beating it.

What does work is Okular's **own full-screen mode**, which hides the menubar and
the toolbar together. shepherd asks for it in the generated `okularrc`:

```ini
[Desktop Entry][$i]
FullScreen=true
shouldShowMenuBarComingFromFullScreen=false
shouldShowToolBarComingFromFullScreen=false
```

The second and third lines are the half that makes it stick. shepherd's
compositor refuses the fullscreen surface state — that is what keeps the HUD
visible — so Okular leaves the mode again immediately, and on the way out it
restores exactly what those keys say, which is nothing. The window keeps its
ordinary geometry inside the HUD's exclusive zone throughout, because the
fullscreen state was never granted.

One consequence for anyone editing the restrictions: **the `fullscreen` action
must stay unrestricted.** Hiding the chrome hangs off that action, so a Kiosk
restriction on it puts the toolbar back.

### What is left

The book, and shepherd's HUD above it. Okular still draws a hairline frame
around each page, which has no setting and reads as a page edge anyway.

## What this is not

The restrictions are a supervision tool, not a security boundary. A child who
can reach a terminal, or an activity that can open one, is outside all of it —
which is what `[entries.firewall]`, per-activity filesystems (#105) and the
rest of the kiosk are for. What they do achieve is that a child using the
reading activity as intended cannot wander out of the book by tapping things.

Two known gaps, both benign on a kiosk:

- **Drag and drop** onto the window would open another document. There is
  nothing to drag from: no file manager, no second application.
- **`[KDE URL Restrictions]`** does not bound what Okular opens. Okular never
  calls `KAuthorized` itself — the coverage above comes entirely from
  `KActionCollection` — so that group constrains KIO callers only, and shepherd
  does not rely on it.

## Reading as a reward

Reading is the obvious thing to bank game time with (issue #8). On the *game*
entry:

```toml
[entries.tokens]
from = ["the-hobbit"]
earn_ratio = 1.0
minimum_seconds = 600
```

Ten minutes of reading unlocks the game, and every further second read banks a
second of play. The reading entry itself carries no limit.

## Other formats, other readers

Okular covers PDF, EPUB, CBZ/CBR/CB7/CBT, DjVu, FictionBook, Markdown,
PostScript, DVI, XPS and TIFF, which is the whole of what this kind is for.

Two readers worth knowing about if Okular does not suit a particular device:

- **zathura** — no menubar, toolbar or context menu at all, so on a
  keyboard-less device there is nothing to lock down. Ubuntu packages it
  without the mupdf backend, so no EPUB: PDF, CBZ, DjVu and PostScript only.
- **calibre's `ebook-viewer`** — the best EPUB typography of the three, and the
  one to reach for if a heavily styled book renders badly. It is **not**
  lockable: tapping the page opens a menu with a file dialog, calibre's *Edit
  book* application, developer tools and a web search. Only for a device where
  that does not matter.

Both would be `type = "process"` entries today, with none of the generated
configuration above, and neither gets the polite close — so neither remembers a
page reliably at the end of a session.

## Troubleshooting

**"The document could not be opened" on an EPUB.** `okular-extra-backends` is
not installed. `sudo shepherd-admin apps install okular`.

**The child is back at page one.** The reading position is written on a clean
close. Check that the session ended through shepherd (the HUD, a time limit, or
`stop`) rather than the process being killed — and that the entry is
`type = "ebook"`, since a `type = "process"` Okular gets no polite close.

**The text is too small.** Raise `font_size`, before the child is far into the
book — it repaginates and moves their place. On a landscape screen, check
`layout = "facing"`: a single portrait page is fitted to the screen height and
letterboxed.

**Nothing happens when the child swipes.** A paged layout has no touch gesture
that turns a page — see "Touch". Use the HUD's `‹` `›` buttons, or set
`layout = "scroll"`.

**The HUD's page buttons do nothing.** They synthesize a keypress through
`/dev/uinput`; if the session user cannot write it, they are inert and the HUD
logs a warning at the first press. Give that user the same uinput access the
input-compat bridges need.

**A toolbar, menubar or sidebar is showing.** The generated configuration is re-rendered on
every launch, so this means the entry is not `kiosk = true`, or is not an
`ebook` entry at all.
