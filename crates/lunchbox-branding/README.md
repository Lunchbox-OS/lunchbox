# lunchbox-branding

The Lunchbox look, as Rust: one crate between
[`assets/branding/tokens.json`](../../assets/branding/tokens.json) and the two
GTK4 front ends that wear it.

## Why it exists

`tokens.json` is the hand-off from the design canvas (issue #207) and the one
place a colour, a radius or a type size is decided. `build.rs` reads it and
writes a `tokens` module into `OUT_DIR`; `lib.rs` includes it. Nothing
downstream restates a value from the design file — it names the token instead,
and a change in the file moves every consumer or fails the build.

There are two consumers, which is the whole reason this is a crate rather than a
module: the launcher (`lunchbox-launcher-ui`'s `theme.rs`, issue #207) and the
HUD (`lunchbox-hud`'s `theme.rs`, issue #209). The launcher owned the codegen
first and the HUD would otherwise have had to copy it.

## What is in it

Every token, twice, named after its own key:

| Kind | Example | For |
| --- | --- | --- |
| Typed constant | `tokens::SPACE_HUD_H: i32`, `tokens::STROKE_KEYLINE: f64` | Code that measures and draws |
| Colour, both spellings | `tokens::COLOR_INK: &str`, `tokens::COLOR_INK_RGB: (f64, f64, f64)` | CSS, and cairo / GSK |
| Substitution table | `tokens::CSS` | `@name@` placeholders in a stylesheet |

A token that is neither a number nor a plain `#rrggbb` — a gradient, a font
stack, a shadow recipe, `120ms` — reaches the stylesheet through `CSS` and gets
no constant.

Plus the two text passes every stylesheet built from the file needs:

* `resolve_tokens` replaces `@name@` with its token, and panics on a name the
  design file does not define — GTK drops an unparseable rule silently, so a
  typo would otherwise reach a screenshot rather than a build.
* `scale_px_literals` multiplies every `<digits>px` literal, which is how both
  front ends follow an output size GTK CSS has no unit for. **The rule that
  comes with it: a length that should scale has to be written in `px`.**
* `stylesheet` does both, in that order — a token carries a bare number and the
  stylesheet spells the unit, so substituting second would leave every
  token-derived length at its design size.

## What is *not* in it

The shape of a stylesheet, anything the design file does not decide, and every
place an implementation departs from the design. Those stay with the crate that
departs, next to the comment saying why.
