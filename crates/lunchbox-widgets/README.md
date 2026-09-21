# lunchbox-widgets

GTK4 widgets that more than one lunchbox surface draws.

The launcher (`lunchbox-launcher-ui`) and the HUD (`lunchbox-hud`) are separate
binaries with separate stylesheets, and almost nothing they show is shared —
deliberately, because they are looked at from different distances and answer
different questions. This crate is where the exceptions live: a widget both of
them draw, where two copies would drift apart.

It is GTK and drawing only. It talks to no daemon, holds no state beyond what
it paints, and depends on nothing in this repository except `lunchbox-util`
(for the clock, which has to respect mock time).

| Widget | What it is |
| --- | --- |
| `ClockFace` | A flat round clock face: a rim and two hands. The HUD's vertical bar shows the wall clock with one; a launcher compartment's floor points one at the hour the category shuts. |

Because these are drawn rather than styled, a widget takes its **size** as a
number from the caller and its **colour** from CSS (`color` inherits, so a
widget dropped beside a label is already the label's colour). See
`src/clock_face.rs` for the long version.
