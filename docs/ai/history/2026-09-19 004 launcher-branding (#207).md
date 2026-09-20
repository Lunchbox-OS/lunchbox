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

## What it looks like

Screenshots in `2026-09-19-004-launcher-branding/` beside this note. The first
two are `config.example.toml` on a development machine — which is why most
activities wear the theme's generic fallback icon rather than their own: Steam,
RetroArch, GCompris and the rest are not installed here. The third is a
synthetic fixture, and says so.

![The home screen at 1280x720](2026-09-19-004-launcher-branding/home.png)

The design size, and what a device actually shows: three compartments fit, the
row overflows, and the yellow chevron says so.

![All five categories](2026-09-19-004-launcher-branding/categories.png)

The whole row on a wider output. Every badge in the vocabulary is here — earn on
Books and Learn, `25m` banked and `0/10` still owed on two activities in Play,
`30m` on Watch's header because the gate is the category's, and nothing on
Listen, which is ungated. Play carries the only schedule, so it has the only
floor.

![A locked and an unlocked activity, both selected](2026-09-19-004-launcher-branding/selection.png)

The same activity selected, locked (left) and unlocked (right). The selection is
identical in both; only the icon and the name dim. See "What 'selected' looked
like" below for why that matters.

This one is **not** from `config.example.toml`: it is a throwaway config holding
one `/bin/true` process called "Example Activity", built so that the item to be
inspected is the launcher's own initial selection — see "Driving the launcher
when the harness will not". The controller is the icon theme's generic
`applications-games`, named by that fixture. An earlier cut of this screenshot
labelled the fixture "Celeste", which was worse than useless: it put a real
activity's name on a fake entry wearing an icon Celeste does not have.

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

## Follow-up: the example config, and one real bug it found

Asked afterwards to "reorganize the example config into groups so that
screenshots generated from it demonstrate the branding" — the option declined
at the start of this work, now wanted.

`config.example.toml` had one group, `attention-heavy` ("Games"), holding three
Steam/RetroArch entries. It is now five categories — Books, Learn, Play, Watch,
Listen — with thirteen more entries assigned. The old group was *renamed* to
`play` rather than replaced, so it keeps its schedule, its 15-minute burst cap,
its combined hour-a-day quota and its cooldown: the only group with limits, and
still the file's worked example of a shared budget. The other four carry an `id`
and a `label` and nothing else, which is all a category needs.

**No entry moved in the file.** It is still ordered by `[entries.kind]` under
its `## ===` headers, because that ordering is what teaches a reader the kinds.
Category order comes from the `[[groups]]` block and member order from the
entries, so the screen could be arranged without disturbing the lesson.

One semantic change, deliberate: Celeste's gate was
`from = ["tuxmath", "gcompris", "scummvm-putt-putt"]` and is now
`from = ["group:learn"]`. It reads better ("time on anything in Learn earns
Celeste time"), it survives a new learning activity being added, it exercises
the `group:` subject prefix that the config docs describe but nothing used, and
it is what puts the earn pill on the Learn *compartment* rather than repeating
it on each of Learn's members. Putt Putt stops being a source, which is right
now that it sits in Play.

### The bug the screenshot found

At 1280x720 the Play compartment was clipped along the bottom edge. Three rows
plus a header plus a floor did not fit, because the item cell had drifted to
178px against the 150px `ITEM_H` the geometry is built on: the art slot, the
name and the badge added up to 150 *before* the item's own 6px padding and 4px
focus border, and those are inside the cell, not outside it.

Trimmed back — the name reserve from 46 to 40, the item padding from 6 to 3, and
the compartment header and floor margins from 12/10 to 8/8. Worth remembering
that `ITEM_H` is the whole cell including its outline, and that a compartment's
worst case is fixed by construction: three rows is the maximum a stack holds, so
three rows plus a header plus a floor has to fit the design's shortest screen,
and if it does not the row clips rather than scrolls.

### Books, and the two things actually keeping it off screen

The category would not render, and the reason was twice not what it looked like.

The example's only book was `~/Books/the-hobbit.epub` — a copyrighted title the
config tells the reader to supply and ships no file for. It is now two Project
Gutenberg books instead, Alice's Adventures in Wonderland and The Wonderful
Wizard of Oz, both public domain, with the two `curl` commands that fetch them
in a comment above. The example is reproducible now: anyone can run those two
lines and get the screenshot.

That was necessary and not sufficient. With the files in place the compartment
still did not appear, and the obvious suspect — `EbookReaderMissing`, okular not
being installed — was the wrong one: that is a *Warning* diagnostic and blocks
nothing. The actual reason on both entries was `ProtectionUnavailable`. A book
declares `[entries.firewall] default = "deny"`, this host had no firewall
helper, and an activity whose protection cannot be applied does not launch. The
launcher hides that rather than dimming it, by the rule above — the child cannot
act on it — so the category emptied and was dropped.

`scripts/integration-tests/setup-firewall-dev.sh`, which the diagnostic's own
remedy names, fixes it. Worth knowing before spending time on a missing reader:
**read the reason code, not the most plausible-looking diagnostic.** Both were
attached to the same entry and only one of them mattered.

### What the screenshots show

All five categories, the mockup's own set. Books (two members) and Listen (one)
as single stacks; Learn (four, double-wide) wearing the earn pill on its header
and not repeating it on its members; Play (seven, triple-wide) with locked
members dimmed, Celeste's own `0/10` under its name, and `Until 6:00 PM` on the
floor; Watch (three). On a wide enough output all five sit on screen with no
chevron chip, which is also the check that the chip appears only on real
overflow.

## Follow-up: demonstrating the token gate

The config showed the gate's *locked* half and nothing else: Celeste wearing
`0/10`. The pill that says what the feature is actually for — the deep-teal one
with the banked time remaining, `25m` — never appeared, for two reasons.

**A balance is runtime state, not configuration.** A fresh install is at zero by
definition, so no amount of editing `config.example.toml` produces a bank pill.
It is earned by playing the `from` activities, or granted outright through the
management UI's `adjust_tokens`.

**And Celeste cannot show the unlocked half on a development machine anyway.**
It is a Steam entry, so on a host without Steam it carries `NotReady` whatever
its balance is, and stays dimmed.

So the example gained a *second* gate, on Pokemon FireRed — a RetroArch entry,
which evaluates as available on a plain host. It also shows the other spelling
of a source: Celeste earns from `group:learn`, this one from `["tuxmath"]`, so
the file now demonstrates both. Its comment says in as many words what the
launcher draws at each balance, and points at the management UI as the quick way
to see it without playing twenty minutes of Tux Math.

Driven through `adjust_tokens`, the whole lifecycle is now visible from the
example:

| Balance | What the launcher draws |
| --- | --- |
| 0s | `0/10` on putty, item at 50% |
| 300s | `5/10` on putty, item at 50% |
| 1500s | `25m` on deep teal, item live |

### And one on a category

An entry's gate and a category's are the same feature aimed at different things,
and the launcher draws them in deliberately different places — so the example
now carries one of each. "Watch" is gated on `group:books`: reading earns screen
time.

That one line does three things a per-entry gate cannot show:

- the pill lands on the **compartment header**, not on any member, which is the
  branding's "a category's gate shows on the compartment, an entry's under its
  own name";
- all three members dim and undim **together**, which is what "earning unlocks
  every member at once" actually looks like;
- **Books** picks up an earn pill, because naming `group:books` as the source
  makes the category one. It was the only compartment without a badge.

Both halves verified through `adjust_tokens` on `group:watch`: `0/20` on the
header with all three members at 50%, then `30m` with all three live.

With it, every badge in the vocabulary is on screen at once — earn on Books and
Learn, a `25m` bank and a `0/10` need on two entries in Play, a `30m` bank on
Watch's header, and nothing on Listen, which is ungated.

## Two things a reviewer's eye caught that mine had not

**The row's edges did not fade.** Layout rule 5 and §7 both call for the
overflowing edge to fade under the chevron chip; only the chip was built, and
the acceptance line "fade + chip appear only on overflow" was ticked off on the
strength of half of it. A compartment clipped by the viewport ended on a hard
vertical cut. `.lb-field__fade` is now a pair of gradient overlays, enamel at
the outer edge to nothing inwards, shown on exactly the condition the chips are
and sitting under them.

Worth knowing for anything else written against this palette: the far stop is
`rgba(39, 160, 147, 0)` and not `transparent`, because `transparent` is
transparent *black* and a gradient interpolating to it dips grey on the way out.

**The chevron chip was white, not yellow** — and had been all along, in every
screenshot taken before this. `.lb-more` set `background-color` but not
`background-image`, and the GTK theme paints a button's own gradient straight
over the colour. `.lb-item` and `.lb-button` had already been given
`background-image: none` for exactly this reason; the chip was missed. Any
control the branding recolours needs both.

## Two places the implementation departs from the mockup

Both asked for after seeing it running, both deliberate, and recorded here
because the reference images in the issue still show the older arrangement.

**A category's badge sits at the end of its header**, not tucked against the
name. Across a row of compartments of different widths, name-adjacent badges
land at a different offset in each one; at the end they line up with each
compartment's own right edge.

The catch was in the layout, not the look: the name label takes the slack to
push the badge over, and GTK computes a widget's expansion from its children
unless the widget states its own — so that propagated out to the row and every
compartment stretched to fill the viewport. A category's width would then have
come from how much screen was spare rather than from how many stacks it holds,
which is the one thing §3 of the brief fixes. It only showed on a wide output;
at 1280 the row overflows and there is no slack to reveal it. The compartment
now states `set_hexpand(false)` for itself.

**An activity's badge rides the icon's top-right corner**, the way a
notification count does, rather than taking a row under the name. The brief's
layout gives every item a badge row whether or not it has a badge — and it has
to, since a reservation that appears only on badged items would push their
neighbours out of line. Most activities have no badge, so that row was height
spent on nothing for nearly all of them. Overlaid, the reservation disappears
and the cell drops from 150px to 132px.

The badge is deliberately not clipped to the icon: a `10/30` pill is slightly
wider than the 78px art slot, and the item has 41px of slack each side plus the
16px column gap, so it has room to hang over the corner without reaching the
next item.

## What "selected" looked like, and two things it turned up

Looking at a *badged* item while selected — which nothing had done until it was
asked for — found two contrast faults.

**A locked item's selection was washed out.** `opacity: 0.5` sat on the whole
cell, so it took the selection down with it: the yellow fill became pale butter,
the 4px ink outline went mid-grey, and the badge faded with everything else.
Moving the D-pad across a compartment of locked activities gave almost no "you
are here" — and the brief's own checklist says focus is always visible.

The 50% now applies to the *activity* — its icon and its name — and not to the
cell. Focus reads identically whether or not the thing under it can be launched,
and the badge stays at full strength, which is what the branding means by a
locked item *keeping* its badge: it is the one part worth reading there.

**A yellow badge on a yellow selection dissolved.** The earn pill and the
selection fill are the same `#FFD166`, so a selected item wearing one kept its
ink outline and its coin but lost its body: a hole punched in the selection
rather than a badge on it. Selected, the earn pill's body is now cream.

Only that pill needs it — bank is deep teal, need is putty, and both stand off
yellow by themselves. The case is not reachable from the example config, where
categories carry the earn pill and a compartment header is not selectable, but
it is reachable from any config that names an entry rather than its group as a
token source.

### Driving the launcher when the harness will not

Neither the pointer nor the keyboard is dependable here. The pointer never
reaches the app at all — `headless.sh` documents that its synthetic pointer does
not fire GTK `clicked`, and motion does not arrive either, so hover cannot
select. The keyboard arrives intermittently: a poll that presses a key and
diffs two screenshots finds a live window, but keys are still dropped inside it,
and `Return` never arrives at all.

What worked instead was making the state deterministic and taking the keyboard
out of it: a scratch config where the item to be inspected *is* the launcher's
initial selection. The focus rule is "previous, then last launched, then the
first launchable item, then simply the first" — so a config in which nothing is
launchable selects its first item, and one where exactly the intended item is
launchable selects that. Two boots, no keypresses, both states captured.

### Talking to the daemon by hand

Worth writing down, because the first attempt failed silently and looked like a
refused connection. **The management socket is not JSON-RPC 2.0**, despite the
shape. The frame is newline-terminated
`{request_id, api_version, method, params}` — see `Request` in
`crates/lunchbox-api/src/commands.rs`. A `{"jsonrpc": "2.0", "id": 1, …}`
envelope deserialises to nothing, and the daemon simply never answers: the
connection stays open, and the client blocks until its own timeout. There is no
error frame to read, so the symptom is indistinguishable from the peer check
having rejected you.

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
