# lunchbox-widgets

GTK4 widgets that more than one lunchbox surface draws.

The launcher (`lunchbox-launcher-ui`) and the HUD (`lunchbox-hud`) are separate
binaries with separate stylesheets, and almost nothing they show is shared —
deliberately, because they are looked at from different distances and answer
different questions. This crate is where the exceptions live: a widget both of
them draw, where two copies would drift apart.

It is GTK and drawing only. It talks to no daemon and holds no state beyond
what it paints; it depends on `lunchbox-util` (for the clock, which has to
respect mock time) and `lunchbox-api` (for the entry an icon belongs to).

| Widget | What it is |
| --- | --- |
| `ClockFace` | A flat round clock face: a rim and two hands. The HUD's vertical bar shows the wall clock with one; a launcher compartment's floor points one at the hour the category shuts. |
| `IconArt` | An activity's icon with a keyline traced around its silhouette — the icon drawn eight times in the outline colour on a ring around itself, which dilates the alpha channel and so works on any paintable. The launcher draws one at 64 px in a compartment; the HUD draws one at 34 px beside the name of the running activity. |

`resolve_icon` comes with `IconArt`: it works out what to draw for an entry — a
file on disk, a theme icon, or the fallback for its kind.

Because these are drawn rather than styled, a widget takes its **size** as a
number from the caller and its **colour** from CSS (`color` inherits, so a
widget dropped beside a label is already the label's colour). That split is why
`IconArt` can trace an ink keyline on the launcher's cream compartment and a
cream one on the HUD's ink bar without knowing about either: a keyline separates
the icon from what is behind it, and each stylesheet says what that is. See
`src/clock_face.rs` for the long version.
