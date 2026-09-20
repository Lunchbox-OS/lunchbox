# Launcher branding (#207)

Building the launcher home screen to the "enamel tin" branding delivered with
<https://github.com/aarmea/lunchbox/issues/207>.

## The prompt

> Implement #207

Issue #207 ("Implement launcher branding") is two attachments and no prose: a
1440p mockup of the launcher, and `lunchbox-branding.zip` — a design hand-off
containing the mark, `tokens.json`, a reference `launcher.css`, mockups, a
`README.md` explaining the direction, and an `IMPLEMENTATION.md` briefing an
agent on what to build. The brief is reproduced at the end of this note, because
it is the specification this work was measured against and it is not otherwise
in the repository.

## Scope, as agreed before building

The brief is addressed to "an agent implementing the home screen and HUD in
`lunchbox-webui`", which is not where either lives — the launcher is GTK4
(`crates/lunchbox-launcher-ui`) and the HUD is its own crate. Three decisions
were put to the author before any code was written:

1. **Launcher home screen only.** The HUD is a mature 3.4k-line crate with
   horizontal and vertical layouts, scaling and flyouts; rebranding it is its
   own issue. It is untouched here and still wears its old palette, so a
   screenshot of the running system shows a branded field under an unbranded
   bar. That is expected, not an oversight.
2. **Categories come from `[[groups]]`,** in config order, with ungrouped
   entries falling into a trailing "Everything else". The alternative — adding
   label-only groups to `config.example.toml` so a fresh install looks like the
   mockup — was declined. The example config therefore renders as "Games" plus
   one wide catch-all, which is honest to the config rather than to the mockup.
3. **Baloo 2 is vendored** in `assets/fonts` under the OFL. No Ubuntu release
   packages it and the kiosk is offline by default, so there is nothing to fetch
   it from at first boot.

## What was built

`crates/lunchbox-launcher-ui` gains five modules and loses `tile.rs`:

| File | What it is |
| --- | --- |
| `theme.rs` | The palette, the geometry, and the stylesheet. The Rust half of `assets/branding/tokens.json`. |
| `field.rs` | The row of compartments: categorisation, the D-pad model, horizontal scrolling. |
| `compartment.rs` | One sunk category well: header, badge, stacks, floor. |
| `item.rs` | One activity: icon with an ink keyline, name, badge, press and shake animations. Replaces `tile.rs`. |
| `badge.rs` | The time pills, and the coin glyph that leads them. |

`grid.rs` keeps its flow box, now built from the new item widget. It is
administrator mode's application picker — a searchable list of every `.desktop`
file on the host, for a caregiver — and a row of compartments is the wrong shape
for that.

### API additions

Three, all driven by something the field has to draw and could not otherwise
know:

- `ServiceStateSnapshot::groups` — the categories, riding the same snapshot as
  the entries. A separate `list_groups` call would have doubled the round trips
  on every `StateChanged` *and* let a compartment's badge disagree with the
  items sitting in it.
- `GroupView::window_closes_at` — for the compartment floor. Distinct from
  `max_run_if_started_now`, which is the shortest of every limit and so says
  when the child must stop, not when the category shuts.
- `EntryView::earns_tokens` / `GroupView::earns_tokens` — for the earn pill. A
  gate names its sources, so a source carries no record of being one; no client
  can derive this, and `Policy::earns_tokens` sweeps the gates to answer it.

## Decisions worth keeping

**Locked activities are now drawn.** The launcher used to skip every entry with
`enabled == false`. It now draws them at 50 % with their badge, because a child
who cannot see Celeste cannot learn that ten minutes of Tux Math would open it.

That is not unconditional. `item::is_shown_when_locked` splits reasons into ones
the child can act on — a closed window, a spent quota, a cooldown, an unmet
gate, a gamepad to plug in — and ones they cannot: a disabled entry, a kind this
host cannot run, a protection that cannot be applied. The second group stays
hidden, as before, because a permanently dead icon in the tin teaches a child to
ignore dimmed items. A new `ReasonCode` has to be classified there; the match is
exhaustive so it cannot be forgotten.

**The selected look is a CSS class, not `:focus`.** Two reasons. The brief wants
hovering to *be* selecting, and one class expresses "exactly one item is
selected" where `:focus, :hover` is two states that can both be true on
different items. And `:focus` did not actually paint under the headless
compositor — `grab_focus()` returns true and GTK agrees the widget is focused,
but the pseudo-class never matched. Focus is still grabbed, for the focus chain
and for screen readers; it is just not what is drawn from.

**The ink keyline is eight GSK colour-matrix draws, not four drop-shadows.** The
brief offers `drop-shadow()` filters. Eight offsets around a 2.5 px ring, each a
colour-matrix node that turns the icon into a solid ink silhouette, work on any
paintable — theme SVG, PNG on disk, pixel art — without touching pixels. Four
offsets give a plus-shaped keyline that visibly corners on round icons.

**Rows reserve space they may not use.** An item's name gets a fixed two-line
box and its badge a fixed slot, whether or not either is filled. Otherwise a
name that wraps pushes its badge down and takes the stack beside it out of
alignment, and the branding is explicit that height never grows — width does.

**The compartment-wide dim was built and then removed.** A shut category dimming
to 40 % is §9 of the brief, which explicitly leaves bedtime to be designed
rather than guessed at. Built, it also dimmed the badge that explains the lock,
which is the one thing worth reading on a locked category. Items carry their own
50 %, which is what the acceptance checklist actually asks for.

## The font, and the hour it cost

Baloo 2 rendered as a plain bold sans for a long time, with no CSS parse error
and `fc-match` finding the font perfectly from the same shell, the same `$HOME`,
and the same environment as the launcher process.

It was a **stale fontconfig cache**. `fc-cache -f <dir>` on the newly-created
font directory was not enough; the global cache had no reason to look at a
directory that did not exist when it was built. A plain `fc-cache -f` fixed it
instantly, and the font has worked everywhere since — including from the user
font directory that appeared not to work before.

Both install paths therefore run `fc-cache` and say so in a comment, because the
failure is silent and looks exactly like a font that was never installed:

- `install_fonts` in `scripts/lib/install.sh` → `/usr/share/fonts/truetype/lunchbox`,
  from `install_system`, so both the from-source install and the `.deb` staging
  get it. The `.deb` gets the cache refresh free from dpkg's fontconfig trigger.
- `install_branding_fonts` in `scripts/lib/deps.sh` → symlinks into
  `$XDG_DATA_HOME/fonts/lunchbox` for `deps install run` and `deps install dev`,
  needing no root, for a stack run out of `target/debug`.

The launcher names Baloo 2 first in a fallback stack, so a device that has
missed the step still comes up — in ordinary lettering.

## Verified on screen

Through the headless dev session (`dev headless --config …` → `dev shot`),
against a scratch config written to put every compartment state on screen at
once — an earning category, a token-gated one, a scheduled one, locked members,
and enough categories to overflow the row:

- Five compartments in config order, sunk, full height, no drop shadow.
- Earn pill on the earning category and *not* repeated on its members; the
  gated category's `0/10`; Celeste's own `0/10` under its name.
- `Until 11:59 PM` on the scheduled category's floor.
- Locked members at 50 % keeping their badges.
- Rows aligned across stacks whether or not a name wraps or a badge is present.
- The row overflowing with the yellow chevron chip, and no vertical scroll.
- Baloo 2 ExtraBold throughout.
- Selection moving with the arrow keys within a stack and across compartments,
  exactly one item selected at a time, pointer hover producing the same state.

**Not verified end to end: the press-then-launch keypress.** The headless
harness could not deliver it. `scripts/lib/headless.sh` already documents that
its synthetic pointer does not fire GTK `clicked`; separately, `wtype -k Return`
(and `KP_Enter`, and `space`, and typed text) exits 0 but never reaches the
launcher's key controller, while arrow keys do — confirmed by logging every key
the controller sees. So the selection, the refusal path and the rendering were
checked on screen, and the launch itself was not. The Enter/space binding is the
same set of keysyms the old grid had, routed to the field instead; what is new
and unexercised is the 120 ms press animation that precedes the launch call.

## Still open

- The HUD (§8 of the brief) — out of scope by agreement, see above.
- Bedtime (§9). Blocked on the engine as well as on design: a compartment floor
  cannot say "Opens 10:00 AM" until `next_window_start` is computed.
  `ReasonCode::OutsideTimeWindow` has carried it since the beginning and nothing
  has ever filled it in — there is a `// TODO: compute next window` in
  `Engine::evaluate_entry` and a matching `None` in `group_reasons`.
  `compartment::schedule_line` is the one line that will want it, and
  `a_shut_category_has_no_floor_yet` is the test that will change.
- Category header icons for pre-readers, and book cover art (§9).

---

## Appendix: the brief, as delivered

Reproduced verbatim from `branding/IMPLEMENTATION.md` in the issue's
`lunchbox-branding.zip`. Its references to `lunchbox-webui`, to `launcher.css`
and to the `reference/*.png` mockups are the hand-off's own; see "Scope" above
for what was done with them.

<!-- The brief refers to a `--lb-scale` CSS variable and to `.lb-item__art`
     etc. from its reference stylesheet. Those class names survive in
     `theme.rs`; the CSS file itself does not, because GTK4 CSS is not the
     web's. -->

```markdown
# Launcher implementation brief

For an agent implementing the home screen and HUD in `lunchbox-webui` to the
branding in this folder. Read `README.md` first for the *why*; this file is the
*what*. `tokens.json` and `launcher.css` carry every number; don't retype them.

## 1. Scope

- Home screen (the "field" of compartments) and its D-pad / pointer / touch
  behavior.
- HUD bar: launcher state and in-activity state.
- Selection and press states.
- Locked / gated presentation, per the token-gate feature (`[entries.tokens]`,
  `[groups.tokens]` in `config.example.toml`).
- Out of scope here: the management web UI, the setup/pairing overlay, bedtime
  screen (see §9).

## 2. Data → screen

One **compartment** per category, in config order. A category is a `[[groups]]`
entry; entries with no group go in an implicit trailing "Everything else"
category, or in per-kind defaults (Books / Watch / Listen) if you already derive
those. The sketch used: Books, Learn, Play, Watch, Listen.

Each compartment shows: the group's label at 22/800; a category badge (an
*earn* pill if this group is a `from` source for any gate; a *bank* pill with
the group balance if the group is gated; none otherwise — one pill max, earn
wins over bank if both); its member entries in config order, 3 per stack then a
new stack to the right; and a footer with the group's next closing time today
(omitted when always-available).

Each item shows: the entry's icon at 64 px with an ink keyline, no plate; its
label at 16/800 wrapping to 2 lines max at 150 px; and an item badge — a *bank*
pill when the entry's own balance is at or above `minimum_seconds`, a *need*
pill `have/need` when below. Only when the entry has its own gate; group-level
gates show on the compartment instead. Locked (balance below the minimum, or
outside the window, or quota spent, or cooling down): 50 % opacity, keep the
badge; for cooldown the badge becomes a clock pill "8m".

Minutes in pills: integer minutes, `m` suffix on bank pills (`25m`), bare on
need pills (`5/10`). Cap the need pill at the threshold: once earned, it becomes
a bank pill.

## 3. Geometry (1280×720 logical)

- HUD 56 px. Field below it. Row starts at x=40, y=24; bottom margin 28.
- Compartment width = `stacks × 160 + (stacks−1) × 16 + 2 × 16 + 2 × 4`. Height
  = full field height (all compartments stretch to the same height).
- Gap between compartments 28.
- Item cell 160 × 150; icon slot 78 with a 64 px icon; 8 px row gap; 16 px
  column gap.
- Scale the whole thing with `--lb-scale` for other resolutions (1920×1080 →
  1.5). Don't reflow: the layout is one row at every size; scale keeps the
  ratios.

## 4. Sunk compartment

Exactly `--lb-shadow-sunk` in `launcher.css`. No outer drop shadow. If the web
view can't do multiple inset shadows cheaply, the priority order is: top inner
shadow (10 px, 10 % black) → outer 3 px ring (12 % black) → bottom inner light
line → side shades.

## 5. Ink keyline on icons

Four `drop-shadow(±2.5px …)` filters on the icon's container (see
`.lb-item__art`). This works on transparent PNG/SVG. Two cases need care:

- **Opaque icons** (JPEG, baked background): the keyline traces the rectangle,
  which is fine — it looks like a framed tile.
- **Performance**: four drop-shadows on 15+ icons is fine on desktop GPUs; on a
  Pi or a Wayland software path, pre-render the keyline into the icon cache at
  install time instead (dilate the alpha by 2.5 px, fill with ink, composite the
  icon over). Same result, no runtime filters.

## 6. Focus, selection, press

- **Focus model**: exactly one focused item at all times when the field is
  showing. Initial focus: last launched entry if it's on screen and available;
  else the first available item in the first compartment.
- **D-pad**: left/right move between stacks (across compartments when at an
  edge); up/down within a stack; wrap vertically inside a stack; don't wrap
  horizontally (hitting the end nudges the scroll instead).
- **Selected look**: `.lb-item:focus-visible` — yellow fill, 4 px ink outline,
  18 px radius, 80 ms.
- **Press (A / Enter / tap)**: add `data-pressed` for 120 ms (scale 1.12, rise
  4 px, 5×6 ink shadow, easing `cubic-bezier(0.2, 0.9, 0.3, 1.3)`), then launch.
  Respect `prefers-reduced-motion`.
- **Locked items** are focusable (so the kid can see the badge) but press does
  nothing except a 120 ms shake of the badge; never launch.
- **Pointer / touch**: hover = focus; click = press. No hover-only affordances.

## 7. Horizontal scroll

- The row translates on X; never scroll Y.
- When the focused item is clipped, translate so its compartment is fully
  visible, aligned to the left margin.
- Show `.lb-field__fade` + `.lb-field__more` on the right only while content
  overflows to the right; mirror on the left when scrolled.
- Wheel / two-finger horizontal scroll and touch drag also move the row.

## 8. HUD

Launcher state: `[mark] Lunchbox [No session] …spacer… 3:00 PM [vol] [batt] 97%
[×]`. Use `icon/lunchbox-small-on-dark.svg` at 26 px.

In-activity state: `[mark] [app icon 34 px, keyline] Minecraft 12:40 left
…spacer… [jar pill] 3:12 PM`. The jar pill shows the balance that this session
is *spending* (the entry's own gate, else its group's). During a *source*
activity show the jar that's *closest to its threshold* among the gates it
feeds, filling; on crossing a threshold, swap the pill for a yellow toast
"Celeste is ready" for ~3 s, then back. Existing warning messages from
`[[entries.warnings]]` render as the same toast in yellow (warn) and
putty-on-ink (critical).

## 9. Not covered, decide when you get there

- **Bedtime / nothing available**: proposed as the same row with every locked
  compartment dimmed to 40 % and its floor reading "Opens 10:00 AM"; any
  always-available compartment stays lit. Draw this before building.
- **Category header icons** if the words are dropped for pre-readers.
- **Book covers**: use the reader's cover if available; else the generic book
  glyph on a tinted plate as in the mockups (the one place a plate is allowed).

## 10. Acceptance checklist

- [ ] Compartments are sunk (inner shadow), no drop shadow anywhere on the
      field.
- [ ] 16 px inside every compartment: left, right, top, between stacks.
- [ ] A category with 4–6 members renders as a double-wide compartment; 7–9
      triple.
- [ ] Earn / bank / need pills appear exactly where §2 says, one per slot.
- [ ] Footer only on scheduled categories; text is the *next* closing time
      today.
- [ ] Focus is always visible; press animates then launches; locked never
      launches.
- [ ] Row scrolls horizontally only; fade + chip appear only on overflow.
- [ ] HUD in-activity shows the correct jar and updates each second.
- [ ] Baloo 2 self-hosted; no text below 12 px; 4.5:1 contrast on all text.
- [ ] Screenshot at 1280×720 matches `reference/launcher.png` in structure
      (icons will differ).
```
