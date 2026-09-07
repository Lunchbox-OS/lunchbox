# Pop-out slider controls, and the vertical bar running out of room (#178)

Date: 2026-09-07. Branch: `fix/hud-slider-popouts`.

## The prompt

> recommend some approaches for #178

then, after the options below were laid out:

> hm let's do A, then do the following for *both* vertical and horizontal
> layouts: remove the icon next to the timer, and instead of putting the
> sliders directly in the HUD, have them pop out from the icon (much like they
> behave on other operating systems)

## The issue

<https://git.armeafamily.com/albert/shepherd-launcher/issues/178> —
*"ebook" type with "left" HUD cuts off the page turn buttons*:

> Consider the following configuration:
>
> ```toml
> [[entries]]
> id = "alice-wonderland"
> label = "Alice's Adventures in Wonderland"
> hud_orientation = "left"
>
> [entries.kind]
> type = "ebook"
> book = "~/Books/alice-wonderland.epub"
> ```
>
> On displays less than 720 logical pixels tall, the activity title is
> abbreviated with ellipses but that is not enough to make the page turn
> buttons visible.

Predicted, and left open, by `2026-09-05 002 hud-on-the-side-scope.md`:

> The vertical bar has none of that — it relies on the screen being tall
> enough. […] if a device ever does run out of vertical room, this is the place
> to add the equivalent of `READING_SLIDER_WIDTH`.

## What was actually wrong

The equivalent of `READING_SLIDER_WIDTH` was already there and not working.

`apply_slider_lengths` had asked for a shorter slider during a reading session
since #160, on whichever axis the bar runs along. On the vertical bar that was
`set_height_request(66)` — and `.hud-vertical .volume-slider` carried
`min-height: 80px`.

**A CSS minimum is a floor GTK takes the *maximum* of against the widget's size
request.** The floor won every time, so the whole of the #160 overflow response
was inert down the side of the screen. Worth remembering as a general trap: a
swapped axis rule may state the *axis*, never the *length*, if the length is
also a size request.

The budget, estimated from the CSS floors and size requests and then confirmed
against screenshots — vertical bar, reading session, backlight present:

| Region | px |
|---|---|
| bar padding (`.hud-vertical`, 12+12) | 24 |
| end-session button | 40 |
| battery icon + label | 40 |
| brightness box (button 40 + slider ≥80 + padding) | 132 |
| volume box (same) | 132 |
| analog clock face | 36 |
| `right_box` spacings (8 × 4) | 32 |
| container spacing | 16 |
| time display | 20 |
| **title, floored at 12 chars** | ~96 |
| **page box (2 × 44px touch targets)** | ~80 |
| `left_box` spacings (12 × 2) | 24 |
| **total minimum** | **≈670** |

Add a configured network check and it is ~710 — the 720 line in the issue. The
`flow_append` reversal puts `left_box` at the bottom, so the overflow lands on
exactly the title and the page buttons and spares the end-session button. That
ordering was right; there was simply nothing that yielded.

## Approaches considered

| | Approach | Verdict |
|---|---|---|
| A | Make the existing machinery work: drop the CSS length floor, give the vertical bar its own shorter reading length | Real bug either way; recovers ~130px, not enough on its own |
| B | Screen-height breakpoint — a "short vertical" profile that drops the clock face, the battery label and the title floor below a threshold | Deterministic, testable; a third layout variant to keep in sync |
| C | Priority shedding: measure `container.measure(Vertical, -1)` against the monitor height and shed in a fixed order until it fits | Only option correct across scale factors and host hardware; most code |
| D | Take page-turning off the bar entirely, onto its own layer surface | Better ergonomics, but a product call |
| — | **Pop the sliders out of the bar** (what was chosen) | Recovers ~200px in both layouts and removes the constraint rather than rationing it |

## What was built

Two commits, deliberately in this order.

**1. `fix(hud): let a reading session actually shorten the vertical bar's
sliders`** — approach A, on its own, as the minimal fix for the reported bug.
Droppable and cherry-pickable if the redesign is ever unwanted. It is
*superseded* by the second commit, which was said up front rather than
discovered later: once the sliders leave the bar there is nothing left for a
reading session to shorten.

**2. `feat(hud): open the volume and brightness sliders out of their icons`** —
the redesign. Each bar icon opens a `GtkPopover` holding a toggle, the slider
and the percentage. The clock glyph beside the countdown is gone; a countdown
is self-describing.

What that retired, all of it dead once the slider is a popover child rather
than a bar child:

- every `.hud-vertical` slider rule (a popover keeps its own horizontal axis),
- both `set_inverted` calls (GTK puts a vertical range's minimum at the top),
- `apply_slider_lengths`, `READING_SLIDER_WIDTH`, and the axis branch in the
  sizing — the flyout has one length,
- the `!vertical && !can_turn_pages` juggling of both percentage labels: the
  flyout has room in every case,
- the `auto_updating` re-entrancy guard (see below),
- `volume::get_volume_status` / `brightness::get_brightness_status`, which
  built a Tokio runtime and connected *synchronously on the GTK main thread* to
  seed a slider that is now hidden until clicked. The 500ms timer seeds it from
  cached state instead.

## Two things worth keeping

**`set_active` does not emit `clicked`.** Both toggles hang their RPC on
`clicked` rather than `toggled`, so the update loop can push the real state
back into the button every tick without echoing an RPC — which is exactly what
the `auto_updating` guard existed to suppress. Measured rather than assumed,
because a wrong answer here is an RPC storm: instrumenting both signals and
flapping `set_active` from the timer for 15 seconds produced **20 `toggled`
and 0 `clicked`**.

**The flyouts are rebuilt on every `HudScaleChanged`**, like the confirm
prompts and for the identical #118 reason: they live hidden across the change
and are *measured* (`align_popover_to_button`) just before being shown, and GTK
leaves a hidden widget's style alone. The rule from the #171 notes still holds —
rebuild-on-scale-change is about measurement, not visibility — and measuring is
what puts these on the wrong side of it. Their live state is re-pushed by the
timer within one tick, while they are still hidden.

## Verification

Headless, entry declaring `hud_orientation = "left"`, page buttons forced
visible with the new `SHEPHERD_HUD_DEBUG_FORCE_PAGE_BUTTONS` hook:

| Screen (logical) | Before | After A only | After the flyouts |
|---|---|---|---|
| 853x400 | volume slider itself cut off; clock, timer, title, page buttons all gone | clock and timer back; title and page buttons still gone | everything fits |
| 1280x480 | — | — | everything fits, page chevrons included |
| 1280x600 | — | — | everything fits; flyouts open correctly |

Both flyouts were opened and screenshotted in both layouts. On the horizontal
bar the title stopped ellipsizing entirely — the ~200px the sliders gave back
is more than the name needed.

## Two follow-up nits

Both raised on review of the screenshots, both fixed in a third commit.

- **The page-turn arrows were rotating with the bar** (`⌃`/`⌄` in a column,
  `‹`/`›` across). They point at the *pages*, not at the buttons, and the
  direction a reader thinks in does not rotate when the bar does — so they are
  `‹`/`›` in both layouts now. The original reasoning (a sideways arrow means
  nothing stacked in a column) traded away the more important consistency: a
  child who learns `›` on one device should not have to learn it again.
- **The countdown was not centred on the vertical bar.** A vertical box gives
  every child the bar's full width and `TimeDisplay` packs its label at the
  start, so the default `Fill` left it hard against the left edge while the
  clock face and the rotated title beside it were centred. The title already
  asked for `halign(Center)` explicitly; the countdown now does too. Worth
  knowing generally: on the vertical bar, centring is something each `left_box`
  child has to ask for.

## Harness notes for the next person

- **The headless output comes up at scale 1.5**, so `--size 1280x600` is a
  *853x400 logical* screen. Two of the measurements above were taken at the
  wrong size before this was noticed; `swaymsg -t get_outputs` reports `rect`
  in logical pixels and `current_mode` in physical ones, and
  `swaymsg output HEADLESS-1 mode WxH scale 1` pins both. An issue written in
  logical pixels cannot be checked against `--size` without this.
- **`SHEPHERD_HUD_DEBUG_FORCE_PAGE_BUTTONS=1`** makes the reading layout
  visible without a reader. Both #171 and #178 previously added a throwaway
  override for this and removed it again; it is permanent now (debug builds
  only, and it forces *visibility* only — the button handlers still check the
  real session, so a stray press cannot page an unrelated activity).
- **`SHEPHERD_HUD_DEBUG_CONFIRM_TRIGGER` gained `.volume` and `.brightness`**,
  which open the two flyouts.
- **Env does not survive a re-boot of the session.** `dev headless` was
  re-run mid-investigation without re-exporting the trigger variable, and the
  debug hook silently did nothing — the flyout "failed to open" for a while
  before that was the answer. Export every variable in the same command.
- `nc -U` without `-q1` never returns; every RPC here wants
  `timeout 5 nc -U -q1`.
