# Lunchbox branding

Draft 0.1 — the "enamel tin" direction. Everything here was chosen together on
one design canvas; this folder is the hand-off, delivered with issue #207.

## The idea

Lunchbox is a sectioned tin. The mark is the tin seen from above with the lid
off; the launcher is the same tin at screen size: enamel-teal field, cream
compartments sunk into it, one compartment per category. Time (the token gate,
the schedule) lives on the compartment or the item it applies to, never in a
separate panel.

## Files

| Path | What it is |
| --- | --- |
| `icon/lunchbox.svg` | The mark, full detail. Use at 32 px and up. |
| `icon/lunchbox-small.svg` | Simplified mark for 16–31 px: no lips, no inner outlines. |
| `icon/lunchbox-small-on-dark.svg` | Same, cream ink, for the HUD bar. |
| `icon/lunchbox-small-on-teal.svg` | Same, with an ink ring so the tin doesn't dissolve into a teal field. |
| `icon/lunchbox-mono-black.svg`, `-white.svg` | One-color versions. Knockouts are drawn in white / `#141413`; set them to the paper color when placing. |
| `icon/lunchbox-sticker.svg` | Full mark with a white keyline: die-cut sticker artwork. |
| `icon/lunchbox-{16…1024}.png` | Rendered set. 16 and 24 use the small mark; 32+ the full one. |
| `icon/avatar-1024.png` | GitHub org/repo avatar: the mark on a cream rounded tile. |
| `icon/apps/companion*.svg`, `media*.svg` (+ PNGs) | The two sibling apps, delivered with issue #217. **Companion** (Android parent app): the tin with the packed column replaced by a padlock. **Media** (standalone player): the tin with a single compartment and a large play. Each has `-small` (under 32 px), `-mono-white` / `-mono-black` (one color, wells cut out as real holes; the white one is the Android notification icon), and the three Android adaptive layers `-adaptive-foreground` / `-background` / `-monochrome` (108 dp canvas, mark at 60 dp inside the 66 dp safe zone; `-512.png` is the Play Store listing icon). The apps draw these as VectorDrawables translated from the adaptive SVGs, so a change here has to be made there too. |
| `tokens.json` | Colors, type, strokes, radii, spacing, shadows, motion — one source of truth. |

The typeface is **Baloo 2**, in `assets/fonts` beside this folder, under the SIL
Open Font License (`assets/fonts/OFL.txt`).

The design hand-off also carried reference mockups of the launcher, the
selection states and the icon in context. Those are **not** in this repository:
the app icons in them are third-party artwork (Tux Math, GCompris, ScummVM,
SuperTuxKart, …) included so the mockups looked real, and they are not part of
this branding. They are attached to
<https://github.com/aarmea/lunchbox/issues/207>, and
`docs/ai/history/2026-09-19 004 launcher-branding (#207).md` says what was built
from them.

## Mark

- Teal tin, ink outline, handle tab. Big cream well with a sunk lip and a yellow
  play triangle; the two small wells are "packed": yellow and deep teal.
- Full mark at ≥ 32 px. Under 32 px use the small mark. On teal backgrounds add
  the ink ring (`-on-teal`). The tab is the weakest part at 16 px; if a 12 px
  glyph is ever needed, drop the tab and keep the three wells.
- Clear space: half the tin's width on every side. Don't rotate, recolor the
  wells, or put a face on it.
- Sibling apps change one thing and nothing else: the companion swaps the
  packed column for a padlock (play well stays, so it's still Lunchbox); the
  media app has one compartment and a bigger play. Don't add a third variation
  without a third app.

## Color

| Token | Hex | Role |
| --- | --- | --- |
| enamel | `#2FB5A5` | field, tin body |
| enamel-deep | `#1F7A72` | lid, banked-time pill, packed well |
| compartment | `#F7F5EE` | column cards (sunk) |
| cream | `#FAF9F5` | paper, HUD text on dark, big well |
| ink | `#1C1B18` | the only outline color; type; HUD base |
| yellow | `#FFD166` | "you can": earn pill, selection, play, latch, scroll chip |
| putty | `#E5E2DA` | "not yet": have/need pill, dividers |
| hud | `#141413` | in-activity bar |

Rules: ink is the only outline color. Yellow means *you can*, deep teal means
*banked*, putty means *not yet*. Locked items sit at 50 % opacity and keep their
badge.

## Type

**Baloo 2** (Google Fonts, OFL; self-hosted — the kiosk is offline by default).
800 for anything the child reads, 700 for the HUD. System sans at 600–700 for
secondary lines. No text under 12 px.

| Use | Size / weight |
| --- | --- |
| Wordmark | 64 / 800, letter-spacing −0.01em |
| Category | 22 / 800 |
| Item | 16 / 800 |
| Badge | 14 / 800 |
| HUD | 18 / 700 |
| Footer, captions | 14 / 700 and 13 / 600, system sans |

## Components

**Compartment** — `#F7F5EE`, 4 px ink outline, 24 px radius, *sunk*: inner
shadow under the top lip, light line at the inner bottom, faint shade on the
inner sides, 3 px dark ring outside. No drop shadow, ever — it's a well, not a
card. 16 px padding. Category name + badge at the top; schedule ("Until 6:00
PM") on the floor, on a hairline; no footer when there's no window.

**Item** — the app's own icon at 64 px with a 2.5 px ink keyline traced around
its silhouette. No plate. Name below, centered, max 150 px. Pixel-art sources
smaller than 64 px render with nearest-neighbour scaling.

**Badges** — pills, 3 px ink outline: `+` on yellow (earns), `25m` on deep teal
(banked, ready), `5/10` on putty (have/need, not yet). A coin (yellow disc, ink
ring, clock hands) leads every pill.

**Selection** — focus fills the item cell yellow with a 4 px ink outline and
18 px radius. Press: the icon scales 1.12, rises 4 px, gains a 5×6 px ink offset
shadow, then the activity launches. Locked items can be focused, not pressed.

**HUD** — ink bar, 56 px, cream type. Launcher: mark, wordmark, session chip,
clock, volume, battery, close. In an activity: the app's icon and time left
replace the chip, and the relevant jar sits on the right.

## Layout rules

These are the rules as built. Where one of them now reads differently from the
draft it was handed down as, the departures at the end say what changed and
why.

1. The field is a row of columns, in config order, read down and then right. A
   column is usually one category; two short ones share it when both fit.
2. A category's members fill its columns in reading order — across the top row,
   then the row under it — as many rows deep as the screen has room for. Width
   grows; height never does.
3. Time lives where it applies: a category that earns or has a group jar carries
   the badge next to its name; an item with its own gate carries it under its
   own name.
4. Schedules go on the compartment floor: "Until 6:00 PM" while the category is
   open, "Opens 4:00 PM" while it is shut, each behind a little clock face
   pointing at that hour.
5. Never scroll vertically. If the row overflows by more than half a cell, the
   whole row scrolls horizontally and the edge fades under a yellow chevron
   chip; by less than that, the cells squish and it does not scroll at all.
6. Locked things stay visible at 50 % with their have/need badge.
7. No plates on app icons. The only plates are book placeholders until covers
   exist.

## What is built, and what is not

The launcher home screen is built to this (`crates/lunchbox-launcher-ui`).
Still open:

- **The HUD.** Untouched by #207 and still on its old palette; the bar above is
  the design for it, not a description of it. Tracked as
  <https://github.com/aarmea/lunchbox/issues/209>, which also lists where the
  bar as built and the bar as drawn disagree.
- **Bedtime** — compartments emptied and dimmed, one left open — is described,
  not drawn. Its floor is built now (a shut category says when it opens), but
  what the *screen* does when everything is shut at once is still a question
  for the design canvas.
- **Category icons** for the row headers, if the words are ever dropped.
- **Book cover art**: the two `book` placeholders in the mockups use a generic
  glyph on a tinted plate.

## Where the launcher departs from this

Each was asked for after seeing it running, and each is argued in
`docs/ai/history/2026-09-19 004 launcher-branding (#207).md`:

- A category's badge sits at the **end of its header**, not against the name,
  so badges line up down a row of compartments of different widths.
- An activity's badge **rides its icon's corner** rather than taking a row under
  the name — most activities have no badge, and the row had to be reserved for
  all of them or none.
- There is **no selection until the child makes one**. The rule here is exactly
  one focused item at all times, which assumes a cursor; a touchscreen has
  none, and a standing highlight there claims a choice nobody has made.
- A row that misses fitting the screen by **less than half a cell** squishes
  its cells rather than scrolling. Rule 5 below is right about a row that does
  not fit; it is wrong about one that is forty pixels too wide, where the
  chevron and the fade buy a strip of screen narrower than an icon.
- Two short categories **share a column** of the field, one above the other,
  rather than each taking a compartment the full height of the screen. Rule 1
  below makes a category a column; this makes a column hold more than one when
  they both fit, which is the difference between five categories on one screen
  and five categories on two.
- Members are **dealt across** the columns in reading order rather than filling
  one column before starting the next, so a part-full category leaves its gap
  along the bottom instead of standing an empty column in the tin. Rule 2 below
  describes the filling; the widths it produces are unchanged.
- A compartment is never narrower than **two item columns**. The mockup's
  one-stack categories are as wide as one item, which leaves the header no room
  for a name and a badge at opposite ends of it — and since the name does not
  ellipsize, it is the compartment that stretches, by a different amount for
  every category.
- A stack holds **as many items as the screen has room for**, not three. Three
  is right at 1280×720 and is still what `space.rows` says; but the field
  scales by its narrower axis so as never to reflow, and on a screen taller
  than 16:9 that leaves height under the compartments that a fixed three
  wastes — and on a shorter one it clips. The launcher measures instead.
- A shut category's floor reads **"Opens 4:00 PM"**. The draft left the bedtime
  screen to be designed (§9 below) and the engine could not have drawn it
  anyway: the next opening time was a field nothing filled in. Both are done.
