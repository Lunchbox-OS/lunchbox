//! The Lunchbox branding, as the front ends consume it: the launcher and the
//! HUD in GTK4, and the media app in egui.
//!
//! `assets/branding/tokens.json` is the hand-off from the design canvas and the
//! one place a colour, a radius or a type size is decided (issue #207).
//! `build.rs` turns it into `tokens.rs` in `OUT_DIR`, which this file includes,
//! so **nothing downstream is a number somebody typed twice**: the launcher's
//! `theme.rs` and the HUD's are the two halves of that file, and each names the
//! tokens it needs rather than restating their values.
//!
//! What lives here is only what the design file decides, plus the two
//! text substitutions every stylesheet built from it needs. The *shape* of a
//! stylesheet, and every place an implementation deliberately departs from the
//! design, stays with the crate it belongs to.

include!(concat!(env!("OUT_DIR"), "/tokens.rs"));

/// The mark in one colour, for a dark surface: the lunchbox seen head on, latch
/// up, with a packed well, a play triangle and two compartments.
///
/// White, with the compartments knocked out in `color.hud` — so it is drawn for
/// the HUD bar specifically, and reads as a silhouette with holes in it rather
/// than as a sticker laid on top. That coupling is real: change the bar's colour
/// and the holes stop matching, which `the_mark_is_knocked_out_in_the_bars_own
/// _colour` fails on.
///
/// The colour version (`lunchbox-small-on-dark.svg`) is what the bar wore
/// first. At 26px its enamel, cream, yellow and deep teal come to a few dozen
/// pixels each and read as a smudge; one colour at that size reads as a shape.
///
/// Compiled in rather than read from `assets/branding/icon` at runtime. There
/// is no install step that would put it on a device — `lunchbox install` places
/// the font, because fontconfig needs a file on disk, but nothing else about the
/// branding is a file anybody looks for — and a kiosk that came up without its
/// own mark because a path moved would be a poor trade for 591 bytes.
///
/// Rendering it needs an SVG loader for gdk-pixbuf (`librsvg2-common`, which
/// `libgtk-4-1` recommends and every icon theme already depends on in practice).
/// A front end that cannot rasterize it should say so and carry on without it.
pub const MARK_MONO_WHITE_SVG: &[u8] =
    include_bytes!("../../../assets/branding/icon/lunchbox-mono-white.svg");

/// Replace every `@name@` in a stylesheet with its design token.
///
/// An unknown name is a panic rather than a silent pass-through: it means the
/// stylesheet asked for a token the design file does not define, and a CSS rule
/// containing a stray `@earn-colour@` would be dropped by GTK's parser without
/// a word — the kind of failure that reaches a screenshot rather than a build.
///
/// A token name is lowercase letters, digits and dashes, which is what lets CSS
/// keep its own `@` for itself: `@keyframes blink { ... }` is not a placeholder
/// and passes through untouched. Without that rule the HUD's blink animation
/// swallowed everything up to the next real token.
pub fn resolve_tokens(template: &str) -> String {
    let mut out = String::with_capacity(template.len() + 512);
    let mut rest = template;
    while let Some(start) = rest.find('@') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let name = after
            .find('@')
            .map(|end| &after[..end])
            .filter(|name| is_token_name(name));
        let Some(name) = name else {
            // CSS's own at-rule, or an `@` in a string. Keep it and carry on
            // from the next character, so a later placeholder still resolves.
            out.push('@');
            rest = after;
            continue;
        };
        let value = tokens::CSS
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| *value)
            .unwrap_or_else(|| {
                panic!("the stylesheet wants `@{name}@`, which tokens.json does not define")
            });
        out.push_str(value);
        rest = &after[name.len() + 1..];
    }
    out.push_str(rest);
    out
}

/// The shape of every key in `tokens::CSS`: `color-muted-on-dark`,
/// `type-hud-size`, `space-item-w`.
fn is_token_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Rewrite every `<digits>px` literal in `template` by `factor`.
///
/// Both front ends are on an output whose size they do not choose, and GTK CSS
/// has no unit that follows it, so a stylesheet is written at one design size
/// and multiplied on the way in. The consequence to remember: **a size that
/// should scale has to be written in `px` in the stylesheet**. Anything left to
/// the GTK theme, or given in any other unit, keeps its logical value and so
/// shrinks on screen as everything around it grows. Both crates learned this the
/// hard way; the notes are at the top of their stylesheets.
///
/// Non-px numbers — timings, opacities, rgba components, font weights — pass
/// through unchanged.
pub fn scale_px_literals(template: &str, factor: f64) -> String {
    let bytes = template.as_bytes();
    let mut out = String::with_capacity(template.len() + 128);
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let digits = &template[start..i];
            if template[i..].starts_with("px") {
                let n: f64 = digits.parse().unwrap_or(0.0);
                out.push_str(&((n * factor).round() as i64).to_string());
                out.push_str("px");
                i += 2;
            } else {
                out.push_str(digits);
            }
        } else {
            out.push(c as char);
            i += 1;
        }
    }
    out
}

/// Resolve a stylesheet's tokens and scale it for the output it will be shown
/// on.
///
/// Tokens first, scaling second, and the order matters: a token carries a bare
/// number (`24`), the stylesheet spells the unit (`@radius-compartment@px`),
/// and only once it is `24px` can the scaler see it. Substituting afterwards
/// would drop every token-derived length back to its design size.
pub fn stylesheet(template: &str, scale: f64) -> String {
    scale_px_literals(&resolve_tokens(template), scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only `<digits>px` may be rewritten: opacities, colours, timings and
    /// bare numbers have to survive untouched, because each of them has been a
    /// bug in one front end or the other.
    #[test]
    fn scales_px_literals_and_leaves_other_numbers_alone() {
        let css = scale_px_literals(
            "a { margin: 10px 4px; opacity: 0.50; transition: 200ms; \
             color: rgba(30, 30, 30, 0.95); border: 1px solid #1C1B18; }",
            1.5,
        );
        assert_eq!(
            css,
            "a { margin: 15px 6px; opacity: 0.50; transition: 200ms; \
             color: rgba(30, 30, 30, 0.95); border: 2px solid #1C1B18; }"
        );
    }

    #[test]
    fn a_token_reaches_the_stylesheet_and_then_the_scaler() {
        let resolved = stylesheet(".x { height: @space-hud-h@px; }", 2.0);
        assert_eq!(
            resolved,
            format!(".x {{ height: {}px; }}", tokens::SPACE_HUD_H * 2)
        );
    }

    #[test]
    #[should_panic(expected = "tokens.json does not define")]
    fn a_token_the_design_file_does_not_define_is_a_panic() {
        resolve_tokens(".x { color: @colour-of-magic@; }");
    }

    /// CSS keeps its own `@`. The HUD's critical-time blink is an at-rule, and
    /// reading it as a placeholder would have eaten the stylesheet from there
    /// to the next real token.
    #[test]
    fn a_css_at_rule_is_not_a_placeholder() {
        let css = resolve_tokens(
            "@keyframes blink { 50% { opacity: 0.5; } }\n\
             .x { color: @color-ink@; }",
        );
        assert!(
            css.starts_with("@keyframes blink { 50% { opacity: 0.5; } }"),
            "{css}"
        );
        assert!(css.ends_with(".x { color: #1C1B18; }"), "{css}");
    }

    /// The mark's holes are the bar's own colour, so they read as holes. If the
    /// bar's colour moves and the mark's does not, the mark quietly becomes a
    /// white tile with four dark patches on it — which looks like a rendering
    /// bug and is not one.
    #[test]
    fn the_mark_is_knocked_out_in_the_bars_own_colour() {
        let svg = std::str::from_utf8(MARK_MONO_WHITE_SVG).expect("the mark is text");
        assert!(
            svg.contains(tokens::COLOR_HUD),
            "the mark is knocked out in something other than color.hud ({}); \
             it is drawn for that bar and for no other surface",
            tokens::COLOR_HUD
        );
    }

    /// The two spellings of a colour cannot drift, because one is generated
    /// from the other.
    #[test]
    fn a_colour_is_emitted_as_hex_and_as_components() {
        assert_eq!(tokens::COLOR_INK, "#1C1B18");
        let (r, g, b) = tokens::COLOR_INK_RGB;
        assert_eq!(
            (
                (r * 255.0).round() as u8,
                (g * 255.0).round() as u8,
                (b * 255.0).round() as u8
            ),
            (0x1C, 0x1B, 0x18)
        );
    }
}
