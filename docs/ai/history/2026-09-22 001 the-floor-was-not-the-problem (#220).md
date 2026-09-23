# The floor was not the problem (#220)

<https://github.com/aarmea/lunchbox/issues/220>

## The prompt

> implement #220

The issue, "Reduce used height of "Opens at"/"Until"... time at the bottom of
sections":

> In particular, at a 1280x800 (or 1920x1200 at 1.5x scale), you can have two
> stacked sections where both has such a footer even when one of the sections
> has up to two items and the other has 3 or 4

## What looking at it found

Reproduced in the headless session with
[`fixture.toml`](2026-09-22-001-field-room/fixture.toml), four categories of
2, 4, 3 and 1 items, all with a time window, at 1280x800:

    laying the field out height=700 rows=3 room=648 heights=[246, 386, 386, 246] slots=4

A one-row compartment with a floor is 246px and a two-row one is 386. Standing
them in one column needs 246 + 28 + 386 = 660, and the budget said 648, so
shaving the floor looked like the fix: six pixels off each would do.

But the screenshot ([before](2026-09-22-001-field-room/before-800.png)) has
compartments 700px tall. The field's content box is 700, and `budget` had
already subtracted the field's 24 + 28px of padding from it, which
`LauncherField::height()` had already done. GTK 4's `width()`/`height()` are
the content box, and `.lb-field` is on the field widget itself. Every screen
has been measured 52px short since the row count was first measured rather
than fixed.

With the room counted once, the two categories stand together
([after](2026-09-22-001-field-room/after-800.png)) and the floor did not need
to change. Trimming it would not have moved any other threshold at 800 either:
the next pairing, two two-row categories, needs 800px against 700.

## What fixing it exposed

The 52px had been hiding a second bug. With it gone, the example config at
1280x720 came up with four-row stacks and Play running off the bottom of the
screen, at a field height of 669 on a 672px window.

The first snapshot is laid out against the default-sized 720px window, before
the fullscreen configure and the HUD strip arrive. The field then reported its
contents' height as its own minimum, so a layout made for a taller field held
the window open at that height, and the relayout in `size_allocate` was only
ever handed the same stale size. Before this change the budget's lost 52px was
just enough to keep that first layout under the real screen.

The field now reports no vertical minimum. It is laid out to whatever it is
given, so it has none to ask for. That is its own commit, ahead of the budget
fix, because the budget fix is not safe without it.

## Left alone, and worth doing

`rebuild` also computes the scale from `self.height()`, against a
`DESIGN_HEIGHT` (720 minus the HUD) that *includes* the field's padding. At
1280x720, the design's own size, that comes out at 0.92 rather than 1.0 on
`main` as well as here, so the launcher is drawn 8% small at the size it was
designed for. Same mistake, different consequence, and fixing it enlarges
everything at 720p, so it was not folded into this.
