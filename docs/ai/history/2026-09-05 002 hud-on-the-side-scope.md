# HUD on the side (#171) — investigation

**Prompt:** "investigate #171"

**Issue:** <https://git.armeafamily.com/albert/shepherd-launcher/issues/171>

> On some hardware or activities, it may make more sense for the HUD to be on
> the side rather than the top. This should be configurable both globally and
> per-activity. When enabled, it should behave a lot like just rotating the HUD
> 90 degrees to the left:
> * HUD along the left of the screen
> * X/logout button on the top (but rendered non-rotated so the bottom of the
>   icon is still facing the bottom of the screen)
> * display and volume sliders vertical
> * Activity title rendered sideways
> * Wall clock as analog time
> * Activity time remaining at 2 significant figures (1h, 59m, ... 1m, 59s)

This is a scope/feasibility investigation, not an implementation.

## What exists today

The HUD is one long-lived process (`crates/shepherd-hud`), started once by sway
(`sway.conf:207`), *not* per session. Everything about its geometry is fixed at
startup from CLI flags:

* `--anchor top|bottom` and `--height 48` (`src/main.rs`), consumed by
  `build_hud_window` (`src/app.rs:199`), which anchors the layer-shell surface
  to `Top|Bottom` + `Left` + `Right` and sets `default_height` /
  `exclusive_zone` from the height.
* `sway.conf` passes neither flag — the HUD runs at the defaults. There is no
  HUD section in `config.example.toml`, and no `hud` key anywhere in
  `shepherd-config`. **Config-driven HUD geometry does not exist yet; this issue
  creates that surface from scratch.**

The bar's content is a single `build_hud_content` (`src/app.rs:256`, ~600 lines)
that builds one horizontal `gtk4::Box` with three horizontal children
(`left_box` = page-turn buttons + activity name + time remaining, `warning_box`,
`right_box` = clock, volume, brightness, display toggle, network, battery,
reset, X). A 500 ms `glib::timeout_add_local` at the end of that function
refreshes every widget from `SharedState`.

### The two mechanisms this feature must reuse

1. **Per-activity flags already have a wire path.** `can_turn_pages` (#160) is
   the freshest example and traces exactly the route an orientation field would
   take:
   `RawEntry` (`shepherd-config/src/schema.rs:239`) → `Entry`
   (`shepherd-config/src/policy.rs`) → `SessionPlan`
   (`shepherd-core/src/session.rs:29`) → `CoreEvent::SessionStarted`
   (`shepherd-core/src/events.rs:24`) → `EventPayload::SessionStarted`
   (`shepherd-api/src/events.rs:60`) + `SessionInfo`
   (`shepherd-api/src/types.rs:1039`) → `SessionState::can_turn_pages()`
   (`shepherd-hud/src/state.rs:124`) → a `set_visible` in the 500 ms timer.
   Defaults that depend on the entry kind live on `EntryKind`
   (`confirms_on_close_by_default`, `supports_page_turn`) and are mirrored into
   the web editor by `shepherd-wire-codegen`
   (`shepherd-webui/src/config/model/kind-defaults.generated.ts`).

2. **Live re-layout of the HUD already happens, twice.** `HudScaleChanged`
   rewrites the stylesheet and resizes the surface at runtime (`apply_scale`,
   `src/app.rs:1462`), and the #87 display-mode code re-anchors the layer
   surface onto a different output at runtime with a `set_visible(false)` →
   `set_monitor` → `set_visible(true)` dance (`src/app.rs:1161`). Both are
   precedent for "the HUD changes shape mid-session"; the second is the exact
   trick needed to make the compositor rebuild a layer surface with new anchors.

## Feasibility of each bullet

| Bullet | Verdict | Notes |
|---|---|---|
| HUD along the left | Easy | `set_anchor(Left+Top+Bottom)`, `set_default_width`/`set_exclusive_zone(width)` instead of height. `sway.conf` hardcodes no edge — the exclusive zone does the work, and the `for_window` kiosk rules are edge-agnostic. |
| Sliders vertical | Easy | `gtk4::Scale` implements `Orientable`. **Gotcha:** a vertical GTK range puts its *minimum at the top*; both sliders need `set_inverted(true)` so up = louder/brighter. `apply_slider_widths` becomes a height request. |
| X button on top, icon unrotated | Easy | Reorder the boxes; the icon simply is not rotated. |
| Activity title sideways | **Hardest single item.** | GTK4 **removed `gtk_label_set_angle`** (verified absent from `gtk4-0.9.7/src/auto/label.rs`). Options: (a) `gtk4::Fixed::set_child_transform` with a `gsk::Transform::rotate(-90.0)` — available in the pinned `gsk4-0.9.6`, but `GtkFixed` measures children *unrotated*, so the container needs an explicit swapped size request; (b) a `glib` subclass overriding `measure`/`snapshot` to swap axes and rotate — cleanest, and `TimeDisplay` is existing subclassing precedent; (c) a `DrawingArea` + Pango, which forfeits the CSS-driven scaling and the ellipsizing the label carefully relies on (`app.rs:305-340`). (b) is the recommendation. Ellipsization must survive — the #160 comments explain why a name that pushes the X off the bar is a real failure, and the same applies vertically. |
| Analog wall clock | Medium | No `DrawingArea` exists anywhere in the repo yet; this would be the first. `gtk4::cairo` is available. Needs to honor the HUD scale factor by hand (the README's rule: only `px` literals in `CSS_TEMPLATE` and explicit rescales in the timer follow the factor). |
| Time remaining at 2 sig figs | Easy | New formatter alongside `format_duration` (`src/time_display.rs:122`), selected by orientation. Boundary settled — see Decisions §3. |

## The real design decisions

1. **Where does the global setting live, and who reads it?** The HUD's geometry
   comes from CLI flags today, and the HUD never reads `config.toml` — only
   shepherdd parses config. Two viable shapes:
   * **(a) shepherdd tells the HUD.** New `[service.hud]` block, resolved per
     session by shepherdd, delivered as a field on `SessionStarted` plus a
     `HudOrientationChanged` event and a `get_hud_orientation` RPC for
     (re)connects. This is a direct copy of the `HudScaleChanged` +
     `get_hud_scale` pair, which exists precisely because a one-shot event
     strands a HUD that connected late or reconnected mid-session
     (`shepherd-ipc/src/client.rs:251`, issue #118). Per-activity comes for
     free, and orientation reverts to the global default on `SessionEnded`.
   * **(b) CLI flag only** (`--anchor left`). Trivial, but gives no
     per-activity control, so it does not satisfy the issue. At most it is a
     phase-0 stepping stone that gets the vertical layout landed before the
     config plumbing.

   Recommendation: (b) as the first phase to build the layout, then (a) for the
   config surface. The per-activity half is what forces runtime re-layout.

2. **Runtime re-orientation is the expensive requirement.** Per-activity
   orientation means the bar must flip when a session starts *and* flip back
   when it ends. Two sub-problems:
   * *The layer surface.* Changing anchors on a mapped `gtk4-layer-shell`
     window needs the unmap/remap treatment already used for output switching
     (`app.rs:1161`); expect the same "GTK thinks it's still mapped" trap.
   * *Widget style caching.* Issue #118's lesson — GTK validates style on
     *map* and leaves hidden widgets alone, which is why
     `build_confirm_prompt` is rebuilt rather than restyled on every scale
     change (see the HUD README). Anything built for the horizontal bar and
     hidden across an orientation change carries the wrong geometry. The
     honest options are (i) rebuild the whole content on orientation change —
     simplest and consistent with how the confirm prompt is handled, or
     (ii) build both layouts up front and swap, which reintroduces exactly the
     hidden-widget staleness #118 documents. **(i) is recommended**, and
     argues for factoring `build_hud_content` so widget construction and the
     500 ms update timer are separable — today they are one 600-line function
     with ~40 clones threaded into a single closure, which is the main
     structural cost of this issue.

3. **The scale-factor contract has to be extended, not bypassed.** Every new
   dimension (vertical slider height, rotated label width, clock diameter) must
   either be a `px` literal in `CSS_TEMPLATE` or be rescaled explicitly in the
   timer, per the rule in the HUD README. A rotated/custom-drawn widget takes
   neither route by default; it needs its size fed from the factor by hand, and
   `apply_scale` needs to drive `set_default_width`/`exclusive_zone` on the
   horizontal axis when the HUD is vertical.

4. **`align_popover_to_button` is horizontal-only** (`app.rs:1436`): it shifts a
   `Bottom` popover left so it does not fall off the right edge (#97). On a
   left-edge bar the X is at the top, the popover should drop to the `Right` (or
   `Bottom` with a vertical offset), and the same clipping analysis has to be
   redone — sway does not slide oversized layer-shell popups back on-screen.

   Both the close-confirmation prompt and the new warning popover (Decisions
   §4) need this, so the alignment logic should be generalized once rather than
   duplicated.

5. **Does anything else assume a top bar?** Audited: no. `sway.conf`'s kiosk
   rules are edge-agnostic and rely on the exclusive zone; `shepherd-launcher-ui`
   never reads the HUD geometry and uses a uniform 48px page padding
   (`launcher-ui/src/app.rs:24`), which absorbs a ~48px left bar as well as it
   absorbs a top one; nothing hardcodes 48 outside the HUD.

## Downstream surfaces a config key touches

* `config.example.toml` (must still pass validation — CLAUDE.md).
* `shepherd-config/src/schema.rs` + `policy.rs`, and `RawEntryKind` if the
  default is kind-dependent (e.g. an `ebook` on a portrait panel).
* `shepherd-wire-codegen` regenerates `config.generated.ts`,
  `kind-defaults.generated.ts` and `wire-types.generated.ts`; the web editor
  needs a control in `shepherd-webui/src/config/components/EntryDetail.tsx`
  (`confirm_on_close` at line 165 is the pattern for a per-entry tri-state).
* HUD `README.md` (documents the flags and the scale-factor rules) and the
  `headless-dev` skill, which is how this gets verified — `./scripts/shepherd
  dev headless` → `dev shot` is the only way to see the layout without a
  graphical login.

## Suggested phasing

1. Refactor `build_hud_content` so construction and the update timer can be
   re-run — prerequisite for everything else, and valuable on its own.
2. Vertical layout behind `--anchor left` at 48px: vertical inverted sliders,
   X at the top, page-turn buttons at the bottom, the compact `90m`/`1h` time
   format, and the warning reduced to its icon. Verify with the headless
   harness.
3. Rotated activity title (subclassed widget) + analog clock (`DrawingArea`),
   both wired into the scale-factor path. Warning-message popover, sharing a
   generalized left-edge alignment helper with the close-confirmation prompt,
   and rebuilt-not-restyled per #118.
4. Config surface: `[service.hud]` global + per-entry override, resolved by
   shepherdd, delivered on `SessionStarted` with a `HudOrientationChanged`
   event and a `get_hud_orientation` RPC for reconnects; runtime flip on session
   start/end via full content rebuild + layer-surface remap.
5. Web editor field + `config.example.toml` documentation.

## Decisions (answered 2026-09-05)

1. **Left edge only** for now. The `--anchor` flag keeps accepting
   `top`/`bottom`; `right` is not built. Nothing in the design forecloses it —
   the layout work is mirror-symmetric — but no config or code path for it
   ships.
2. **Bar width stays 48px**, matching the current height, so a vertical HUD
   costs the activity exactly what a horizontal one does.
3. **Time remaining switches to hours at 100 minutes** — minutes for as long as
   they fit two digits, then whole hours. Strictly "2 significant figures": the
   display is never more than three characters.

   | remaining | shown |
   |---|---|
   | 59s | `59s` |
   | 60s | `1m` |
   | 59min | `59m` |
   | 90min | `90m` |
   | 99min | `99m` |
   | 100min | `1h` |
   | 119min | `1h` |
   | 120min | `2h` |

   Note `100min`-`119min` render as `1h`, hiding up to 59 minutes. That is the
   accepted cost of the character budget; the horizontal bar keeps its existing
   `H:MM:SS` and is unaffected.
4. **The warning banner keeps horizontal text, in a popover.** Warning messages
   are operator-authored free text (`config.example.toml:707`: `"10 minutes
   left - start wrapping up!"`), and GTK clips rather than wraps — so at 48px
   the banner cannot render inline. In the vertical bar the warning is the
   `dialog-warning-symbolic` icon alone, carrying the existing severity colour
   and the `warning-critical` blink; the message text drops out of it as a
   popover, laid out horizontally, following the same show/hide lifetime the
   banner has today (`app.rs:1039`-`1096`). The bar keeps its 48px exclusive
   zone; only the popover is wide.

   This makes the warning popover the **second** popover to need the #97
   clipping analysis redone for a left-edge bar (see "The real design
   decisions" §4) — neither GTK nor sway slides an oversized layer-shell popup
   back on-screen, and both of these now drop to the `Right`. They should share
   one alignment helper rather than growing a second copy of
   `align_popover_to_button`.

   It also means the warning popover is a widget that lives **hidden across
   orientation and scale changes**, which is exactly the #118 trap: it must be
   rebuilt, not restyled, like `build_confirm_prompt`.

## Judgement calls taken without asking

* **Config shape mirrors `confirm_on_close`**: a global default under
  `[service.hud]` plus an `Option` per entry, so an entry can override the
  global in either direction rather than only opting in.
* **No kind-dependent default.** Unlike `confirm_on_close` / `supports_page_turn`,
  orientation is a property of the hardware, not of what the activity does, so
  `EntryKind` gets no `default_hud_orientation`. The global setting is the
  default; entries override individually.
* **The global setting applies while idle**, i.e. the launcher gets the
  configured orientation, and a per-activity override applies only for the life
  of the session, reverting on `SessionEnded`.
* **Page-turn buttons move to the bottom** of the vertical bar. Horizontally
  they sit at the far left specifically to be as far as possible from the reset
  and X buttons (`app.rs:283`-`289`); with the X at the top, the bottom is that
  same corner.
* **The analog clock replaces the digital one** in vertical mode rather than
  sitting alongside it; the digital wall clock is unchanged horizontally.


---

# Implementation notes (2026-09-05)

Built in two commits. What the plan got right, and the three places it was
wrong:

## The phasing held, except for phase 1

Phase 1 was "refactor `build_hud_content` so construction and the update timer
can be re-run", and it was expected to be the expensive part — a 600-line
function with ~40 clones threaded into one closure. **It was not needed.** The
timer is registered *inside* `build_hud_content`, so re-running the function
re-registers it; all a rebuild needs is for the *old* timer to stop. A shared
generation counter does that in five lines: the rebuild bumps it, and each
timer compares it to the generation it was built as and returns
`ControlFlow::Break`. The 40 clones stayed exactly where they were.

That is worth remembering as a general shape: when a builder owns its own
update loop, "rebuild" is cheaper than "make the update loop re-targetable".

## Three things the investigation did not predict

1. **Axis-specific CSS was the real sizing bug.** The plan noted that new
   dimensions must go through `scale_px_literals`. What it missed is that
   *existing* rules name an axis — `min-width: 80px` on a slider, `min-height:
   4px` on its trough, `padding: 0 4px` separating a control group. On a
   vertical bar all three are demanded *across* the bar, and the surface
   measured **124px** wide instead of 48. The fix is a `.hud-vertical` block
   that turns each of them.
2. **The bar was already thicker than its exclusive zone.** `--height 48` sets
   the layer-shell zone; the surface is sized to its content, which is 54px
   (a 32px `.indicator-button` plus its 4px padding plus the bar's 6px). So the
   horizontal bar has always overhung its reserved zone by 6px. The vertical
   bar was brought to the same 54 rather than to a nominal 48 — parity with
   what ships, not with what the flag says.
3. **`gtk_widget_get_color` is feature-gated.** The drawn clock face takes its
   colour from CSS, which needs `v4_10` on the `gtk4` crate. Enabling it is
   safe (shepherd requires Ubuntu 26.04, GTK 4.18+) but it is a workspace-wide
   dependency change, not a local one.

## What the plan got right

* The `can_turn_pages` route was exactly the template for the per-entry field.
* The `HudScaleChanged` + `get_hud_scale` pairing was the right model for the
  wire protocol, and for the same reason (#118): the event fires only on
  change, so a HUD that connected late needs to be able to ask.
* `align_popover_to_button` did need redoing, and the warning popover made it
  two popovers rather than one — they now share the helper.
* The #118 hidden-widget trap is why the bar is rebuilt rather than restyled.

## Verified end to end in the headless session

Layout, exclusive zone, inverted sliders, the analog face reading a mocked
2:37, a live session's sideways "Big Buck Bunny" and "60m", the end-session
prompt dropping to the right, a real "5 minutes remaining" warning popover, and
the full config round trip: a `hud_orientation = "left"` entry flipping the bar
on launch (`Rebuilding the HUD ... before=Top after=Left`) and flipping it back
on stop.

## The editor controls

Both are in: `hud_orientation` on an activity's Behaviour tab, and the
device-wide `[service.hud] orientation` in a "HUD" section on the Device page.
The option list and the description of what the vertical HUD *is* live in
`model/hudOrientation.ts`, so the two menus cannot drift, and the list is typed
as `RawHudOrientation` — a new edge added to the Rust schema fails the type
check until it is given a label rather than quietly going missing from both.

Two behaviours the DOM tests in `config/hud-orientation.test.tsx` pin, because
the obvious implementation gets each wrong:

* Clearing the **device** setting unsets the whole `[service.hud]` table rather
  than leaving an empty one. The daemon reads "top" either way; the difference
  only shows in a file people annotate.
* Clearing the **activity** setting unsets the key rather than writing "top".
  "Inherit" and "top" are different answers — an activity pinned to top keeps
  its top bar when the device moves to a side bar, and an inheriting one
  follows.

Those tests also caught a real bug before it shipped: MUI renders a select
whose value is `""` as a blank box, so both menus read as empty when unset
instead of saying "Top (the default)" / "Use the device setting" — which is the
one thing this control exists to tell you. Fixed with `displayEmpty` and a
pinned label. (The pre-existing "Category" select on the same page has the same
blank-when-unset behaviour; left alone, since "no category" reads acceptably as
an empty field in a way "no edge" does not.)

Verified by those DOM tests rather than a screenshot: the standalone editor
loads its validator as wasm, so `firefox --screenshot` catches it mid-load and
a real screenshot would need browser automation this environment has no
selenium for. The tests assert the rendered text of each closed menu in both
states, which is what a screenshot would have shown.


## Under the XWayland DPI hack (tested after the fact)

The crate README's scale-factor rule is the thing a new widget is most likely
to break, and the vertical layout adds several dimensions that have to follow
the factor. Tested at counter-scale 2.0 and 1.5 by raising the headless output
scale and launching an `xwayland_native_resolution` activity.

Everything follows: bar thickness (112px at 2.0, 80px at 1.5, against a nominal
54 × f — per-literal rounding accounts for the drift), icons, the vertical
slider, the rotated title, and the analog clock. The clock is the only one that
needed wiring by hand, being drawn rather than styled. The restore path works,
and a scale change and an orientation change fire together correctly on launch,
with the rebuilt bar re-applying the current factor.

Two findings.

**A hypothesis from reading the code was wrong.** The scale-change path rebuilds
the two confirm prompts and not the new warning popover, which looked like the
#118 hidden-widget trap. It is not. The confirm prompts are rebuilt because
their *measured* width feeds `align_popover_to_button` and a stale style gives a
stale measurement; the warning popover is never measured, only popped, so GTK
validates its style at map time against the current stylesheet. Verified at 2.0
with a scale-only change (device already vertical, so nothing rebuilt) — the
message renders correctly. No rebuild was added.

The general lesson: "rebuild on scale change" in this file is a rule about
*measurement*, not about visibility. A hidden widget that is only ever shown is
fine; one whose size is read before it is shown is not.

**A real pre-existing bug, now fixed.** The page-turn icons added in #160 were
never listed in `scaled_icons`, so under counter-scale they stayed 20px beside
neighbours that doubled — the #114 rule, broken. Latent, since a book is not an
XWayland activity, but real. Fixed in its own commit.

## The vertical bar has no overflow strategy

Worth recording because it is a difference from the horizontal bar rather than a
bug. The horizontal bar has the #160 machinery for running out of room: the
title ellipsizes, the sliders shorten, and the end-session button is protected.
The vertical bar has none of that — it relies on the screen being tall enough,
which on real hardware it is, because the counter-scale only doubles when the
panel's pixel count does.

Forced past it (factor 2.0 on a 720px-tall test screen — a configuration sway
would not produce, since it would not run a 720p panel at scale 2) GTK clips the
bottom of the bar: the activity title and the page-turn buttons. The
end-session button, at the top, survives. That is the right ordering, and it is
what the `flow_append` reversal buys — but if a device ever does run out of
vertical room, this is the place to add the equivalent of `READING_SLIDER_WIDTH`.

## Harness notes for the next person

* `swaymsg output <name> scale N` on a headless output **resets its custom mode**
  to 1280x720. Set both together (`output HEADLESS-1 mode 1280x1600 scale 2`) or
  the tall screen silently goes away.
* Env vars do not persist between shell invocations, so `export FOO=1` and
  `shepherd dev headless` have to be in the same command for the variable to
  reach the sway-spawned clients.
* There is no way to run a real reading activity here (okular is not installed),
  so the page-turn buttons were made visible with a temporary env-gated override
  in the update timer, screenshotted at both factors, and the override removed.
