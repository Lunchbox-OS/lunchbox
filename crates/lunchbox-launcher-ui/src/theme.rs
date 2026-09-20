//! The Lunchbox look: palette, geometry and the launcher stylesheet.
//!
//! Every number here comes from `assets/branding/tokens.json`, which is the
//! hand-off from the design canvas and the one source of truth. Keep the two in
//! step: if a value moves there, move it here, and say so in the note under
//! `docs/ai/history` rather than editing the token file to match the code.
//!
//! The idea the numbers serve: Lunchbox is a sectioned tin. The field is the
//! enamel, each category is a cream compartment *sunk into* it — which is why
//! the compartment carries inset shadows and never a drop shadow — and time
//! lives on the compartment or the item it applies to, never in a panel of its
//! own.

/// The palette, as the components cairo wants.
///
/// Only the three colours that are *drawn* rather than styled live here. Every
/// other colour in the branding reaches the screen through `CSS_TEMPLATE`
/// below, which is the one place they are written; duplicating them as Rust
/// constants nothing reads would only give them somewhere to drift to. The
/// tests at the foot of this file check that these three still agree with the
/// stylesheet.
///
/// Ink: the only outline colour, and the colour of type.
pub const INK_RGB: (f64, f64, f64) = (
    0x1C as f64 / 255.0,
    0x1B as f64 / 255.0,
    0x18 as f64 / 255.0,
);
/// Yellow: "you can" — the coin on an earn or a have/need pill.
pub const YELLOW_RGB: (f64, f64, f64) = (
    0xFF as f64 / 255.0,
    0xD1 as f64 / 255.0,
    0x66 as f64 / 255.0,
);
/// Cream: the coin on the deep-teal bank pill, where yellow would lose its ring.
pub const CREAM_RGB: (f64, f64, f64) = (
    0xFA as f64 / 255.0,
    0xF9 as f64 / 255.0,
    0xF5 as f64 / 255.0,
);

/// The logical width the geometry below is drawn for. A wider output scales
/// the whole field rather than reflowing it: the layout is one row at every
/// size, and scaling is what keeps the ratios (§3 of the branding brief).
pub const DESIGN_WIDTH: f64 = 1280.0;
/// The logical height of the *field* — the 720 px screen the design targets,
/// less the 56 px HUD bar that sits above this window rather than inside it.
pub const DESIGN_HEIGHT: f64 = 664.0;

/// Items in a stack before the category spills into a second stack beside it.
pub const ROWS_PER_STACK: usize = 3;
/// Item cell, unscaled.
///
/// The height is the brief's 150 less the row the badge used to occupy under
/// the name: the badge now rides the icon, so every item is the same height
/// whether or not it has one, and the space goes back to the field.
pub const ITEM_W: i32 = 160;
pub const ITEM_H: i32 = 132;
/// The art slot inside an item, and the icon drawn in the middle of it.
pub const ART_SLOT: i32 = 78;
pub const ICON_PX: i32 = 64;
/// Radius of the ink keyline traced around an icon's silhouette.
pub const KEYLINE: f64 = 2.5;

/// Type size of an activity's name, unscaled.
///
/// Written here as well as in `.lb-item__name` below because the leading is a
/// Pango attribute rather than a CSS rule — GTK4 CSS has no `line-height` —
/// and computing an absolute leading needs the size in Rust. The test at the
/// foot of this file fails if the two spellings drift apart.
pub const ITEM_FONT_PX: i32 = 16;

/// How much to scale the design for an output of `width` × `height` logical
/// pixels. Both axes are considered so a short screen shrinks the row rather
/// than clipping the bottom off it, and the result is clamped: below ~0.75 the
/// 12 px type floor is broken, and there is nothing above 3 to gain.
pub fn scale_for(width: i32, height: i32) -> f64 {
    if width <= 0 || height <= 0 {
        return 1.0;
    }
    let by_width = width as f64 / DESIGN_WIDTH;
    let by_height = height as f64 / DESIGN_HEIGHT;
    by_width.min(by_height).clamp(0.75, 3.0)
}

/// Scale one unscaled length, for the size requests that CSS cannot express.
pub fn px(value: i32, scale: f64) -> i32 {
    ((value as f64) * scale).round() as i32
}

/// Rewrite every `<digits>px` literal in `template` by `factor`.
///
/// The launcher is fullscreen on an output of unknown size, and GTK CSS has no
/// unit that follows it, so the stylesheet is written at the design size and
/// multiplied on the way in. The consequence to remember: **a size that should
/// scale has to be written here in px**. Anything left to the GTK theme, or
/// given in any other unit, keeps its logical value and so shrinks on screen as
/// everything around it grows. (The HUD learned this the hard way — see the
/// note at the top of its own stylesheet.)
fn scale_px_literals(template: &str, factor: f64) -> String {
    let bytes = template.as_bytes();
    let mut out = String::with_capacity(template.len() + 128);
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
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
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

/// The launcher stylesheet, scaled for the output it will be shown on.
pub fn stylesheet(scale: f64) -> String {
    scale_px_literals(CSS_TEMPLATE, scale)
}

/// Written at the design size; see `scale_px_literals` for why every length is
/// in `px`. Colours are spelled out rather than named through GTK's own
/// `@define-color`, so that this file and `tokens.json` are diffable.
const CSS_TEMPLATE: &str = r#"
/* ---------------------------------------------------------------- the tin */

/* The field: enamel with a soft sheen across it, so a 1280 px span of flat
   teal doesn't read as a solid colour swatch. */
window.lb-launcher {
    background-image: linear-gradient(90deg,
        #27A093 0%, #35C2B1 18%, #2FB5A5 50%, #35C2B1 82%, #27A093 100%);
    background-color: #2FB5A5;
    color: #1C1B18;
    /* Baloo 2 is shipped with Lunchbox (`assets/fonts`, OFL) and installed by
       `scripts/lunchbox deps install fonts`, because the kiosk is offline by
       default and no distribution packages it. The fallbacks are what the
       launcher looks like if that step has not been run: the layout is
       unchanged and everything is still legible, only the lettering is
       ordinary. If it renders ordinary when you expect otherwise, the cause is
       almost always a stale fontconfig cache rather than a missing file --
       `fc-cache -f` and look again. */
    font-family: "Baloo 2", "Comic Neue", sans-serif;
}

/* Vertical margins only. The side margins belong to the *row*, not the field,
   so the scroll viewport reaches the screen edge — which is what lets the edge
   fade end exactly where the enamel sheen reaches its own edge stop, and what
   lets a scrolled compartment run off the screen rather than stopping 40px
   short of it. */
.lb-field {
    padding: 24px 0 28px 0;
}

/* The field's side margins, carried by the row so they scroll with it: the
   first compartment starts 40px in, and the last one keeps 40px after it when
   the row is scrolled to the end. */
.lb-field__row {
    padding: 0 40px;
}

/* The edge the row runs out under, at whichever side it continues past.

   Enamel at the outer edge fading to nothing inwards, so a compartment clipped
   by the viewport dissolves into the field rather than ending on a hard
   vertical cut. The colour is the *edge* stop of the field's own sheen
   (#27A093), because that is the enamel this sits on top of.

   `rgba(39, 160, 147, 0)` rather than `transparent`: GTK interpolates a
   gradient through its stop colours, and `transparent` is transparent *black*,
   so the fade would dip grey on its way out. */
.lb-field__fade {
    min-width: 56px;
}

.lb-field__fade--right {
    background-image: linear-gradient(to right,
        rgba(39, 160, 147, 0), rgba(39, 160, 147, 1));
}

.lb-field__fade--left {
    background-image: linear-gradient(to left,
        rgba(39, 160, 147, 0), rgba(39, 160, 147, 1));
}

/* The yellow chevron chip at the edge the row continues past. */
.lb-more {
    /* Held off the screen edge by hand now that the overlay reaches it. */
    margin: 0 20px;
    background-color: #FFD166;
    /* The GTK theme gives a button its own `background-image` gradient, which
       paints straight over a `background-color` and left this chip white. Any
       control the branding recolours has to clear the image as well as set the
       colour — `.lb-item` and `.lb-button` already do. */
    background-image: none;
    border: 4px solid #1C1B18;
    border-radius: 999px;
    min-width: 48px;
    min-height: 48px;
    color: #1C1B18;
    box-shadow: 3px 3px 0 #1C1B18;
}

/* -------------------------------------------------------- the compartment */

/* Sunk, not stacked: an inner shadow under the top lip, a light line along the
   inner bottom, a faint shade down each inner side, and a dark ring just
   outside the outline. No drop shadow, ever — it is a well, not a card. */
.lb-compartment {
    background-color: #F7F5EE;
    border: 4px solid #1C1B18;
    border-radius: 24px;
    padding: 16px;
    box-shadow: inset 0 10px 0 rgba(0, 0, 0, 0.10),
                inset 0 -4px 0 rgba(255, 255, 255, 0.70),
                inset 4px 0 0 rgba(0, 0, 0, 0.05),
                inset -4px 0 0 rgba(0, 0, 0, 0.05),
                0 0 0 3px rgba(0, 0, 0, 0.12);
}

.lb-compartment__name {
    font-size: 22px;
    font-weight: 800;
    color: #1C1B18;
    padding: 0 4px;
}

.lb-compartment__header {
    margin-bottom: 8px;
}

/* The floor: a hairline, then the category's closing time today. */
.lb-compartment__floor {
    border-top: 3px solid #E5E2DA;
    margin-top: 8px;
    padding-top: 8px;
}

.lb-compartment__schedule {
    font-family: system-ui, -apple-system, sans-serif;
    font-size: 14px;
    font-weight: 700;
    color: #5C5A55;
}

/* --------------------------------------------------------------- the item */

.lb-item {
    background: none;
    background-color: transparent;
    background-image: none;
    border: 4px solid transparent;
    border-radius: 18px;
    padding: 3px;
    box-shadow: none;
    outline: none;
    color: #1C1B18;
    transition: background-color 80ms ease-out, border-color 80ms ease-out;
}

/* The selected cell: filled yellow behind an ink outline.

   Driven by a class the field puts on, not by `:focus`. Two reasons, and the
   second is the one that decided it:

   1. The brief asks that hovering *be* selecting, so there is exactly one
      selected item whether the child is using a thumbstick, an arrow key or a
      finger. One class expresses that; `:focus, :hover` is two states that can
      both be true, on different items.
   2. `:focus` did not paint here. The item really does hold the focus --
      `grab_focus()` returns true and GTK agrees the widget is focused -- but
      the pseudo-class never matched under the headless compositor. Rather than
      depend on a pseudo-class whose behaviour varies with how the toplevel got
      its keyboard focus, the launcher styles the state it already tracks. */
.lb-item.lb-item--selected {
    background-color: #FFD166;
    background-image: none;
    border-color: #1C1B18;
    outline: none;
}

/* Locked: still visible, still focusable, still wearing its badge — that badge
   is the whole point, because it says what would unlock it.

   The 50% is on the *activity* — its icon and its name — and deliberately not
   on the cell. Dimming the whole item took the selection down with it: the
   yellow fill washed out to pale butter, the ink outline went grey, and moving
   the D-pad across a compartment of locked activities gave almost no "you are
   here". Focus has to read the same whether or not the thing under it can be
   launched.

   The badge stays at full strength for the same reason it is drawn at all: it
   is the one part of a locked item worth reading, and the branding's own wording
   is that a locked item *keeps* its badge. */
.lb-item.lb-item--locked .lb-item__art,
.lb-item.lb-item--locked .lb-item__name {
    opacity: 0.50;
}

.lb-item__name {
    font-size: 16px;
    font-weight: 800;
    color: #1C1B18;
}

/* -------------------------------------------------------------- the pills */

.lb-badge {
    border-radius: 999px;
    border: 3px solid #1C1B18;
    padding: 1px 10px 1px 4px;
    font-size: 14px;
    font-weight: 800;
}

/* "You can earn here." */
.lb-badge.lb-badge--earn {
    background-color: #FFD166;
    color: #1C1B18;
}

/* The one place two yellows meet. A selected cell is filled yellow, so an earn
   pill on it keeps its ink outline and its coin but loses its body — it reads
   as a hole punched in the selection rather than a badge sitting on it. On
   paper the pill stays a pill.

   Only the earn pill needs this: bank is deep teal and need is putty, both of
   which stand off yellow by themselves. */
.lb-item.lb-item--selected .lb-badge.lb-badge--earn {
    background-color: #FAF9F5;
}

/* "This much is banked and ready to spend." */
.lb-badge.lb-badge--bank {
    background-color: #1F7A72;
    color: #FAF9F5;
}

/* "Not yet": have against need. */
.lb-badge.lb-badge--need {
    background-color: #E5E2DA;
    color: #1C1B18;
}

/* Cooling down. Same shape as the others so the row doesn't jump. */
.lb-badge.lb-badge--wait {
    background-color: #E5E2DA;
    color: #5C5A55;
}

/* ------------------------------------------------- everything that is not
   the field: loading, errors, the administrator picker. Branded, but plainly
   so — none of it is what the child is meant to be looking at. */

.lb-message-title {
    font-size: 28px;
    font-weight: 800;
    color: #1C1B18;
}

.lb-message-body {
    font-family: system-ui, -apple-system, sans-serif;
    font-size: 16px;
    font-weight: 600;
    color: #5C5A55;
}

.lb-card {
    background-color: #F7F5EE;
    border: 4px solid #1C1B18;
    border-radius: 24px;
    padding: 32px 40px;
    box-shadow: inset 0 10px 0 rgba(0, 0, 0, 0.10),
                0 0 0 3px rgba(0, 0, 0, 0.12);
}

.lb-button {
    background-color: #FFD166;
    background-image: none;
    border: 4px solid #1C1B18;
    border-radius: 999px;
    padding: 8px 24px;
    font-size: 18px;
    font-weight: 800;
    color: #1C1B18;
    box-shadow: 3px 3px 0 #1C1B18;
}

.lb-spinner {
    min-width: 48px;
    min-height: 48px;
    color: #1C1B18;
}

.admin-picker { padding: 32px 48px; }

.admin-search {
    font-size: 20px;
    padding: 10px 14px;
    border-radius: 999px;
    border: 4px solid #1C1B18;
    background-color: #FAF9F5;
    color: #1C1B18;
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// A colour written for CSS, from the components cairo draws with.
    fn hex(rgb: (f64, f64, f64)) -> String {
        let c = |v: f64| (v * 255.0).round() as u8;
        format!("#{:02X}{:02X}{:02X}", c(rgb.0), c(rgb.1), c(rgb.2))
    }

    /// The three drawn colours are written twice — once as components for
    /// cairo, once as hex in the stylesheet. This is what stops the two halves
    /// of the palette drifting apart.
    #[test]
    fn the_drawn_colours_are_the_styled_colours() {
        let css = stylesheet(1.0);
        for (name, rgb) in [
            ("ink", INK_RGB),
            ("yellow", YELLOW_RGB),
            ("cream", CREAM_RGB),
        ] {
            let spelled = hex(rgb);
            assert!(
                css.contains(&spelled),
                "{name} is drawn as {spelled}, but the stylesheet never \
                 mentions that colour"
            );
        }
    }

    /// The item name's type size is written twice — once for the stylesheet,
    /// once for the Pango leading that CSS cannot express. They must agree, or
    /// the leading is computed for a size the text is not set at.
    #[test]
    fn the_item_type_size_matches_the_stylesheet() {
        let css = stylesheet(1.0);
        // The block opened by the bare selector on its own line, not any rule
        // that merely mentions the class — the locked state has a compound
        // selector ending in `.lb-item__name` and would otherwise be matched
        // here instead.
        let rule = css
            .split("\n.lb-item__name {")
            .nth(1)
            .and_then(|rest| rest.split('}').next())
            .expect("the item name rule is in the stylesheet");
        assert!(
            rule.contains(&format!("font-size: {ITEM_FONT_PX}px")),
            "ITEM_FONT_PX is {ITEM_FONT_PX}, but .lb-item__name says otherwise: {rule}"
        );
    }

    #[test]
    fn px_literals_scale_and_nothing_else_does() {
        let css = scale_px_literals(
            "a { margin: 10px 4px; opacity: 0.50; color: #1C1B18; }",
            1.5,
        );
        assert_eq!(
            css, "a { margin: 15px 6px; opacity: 0.50; color: #1C1B18; }",
            "only <digits>px may be rewritten: opacities, colours and \
             unitless numbers have to survive untouched"
        );
    }

    #[test]
    fn the_stylesheet_scales_as_a_whole() {
        let one = stylesheet(1.0);
        let two = stylesheet(2.0);
        assert!(
            one.contains("font-size: 22px"),
            "category name at design size"
        );
        assert!(two.contains("font-size: 44px"), "and doubled at 2x");
        // The 999px pill radius is a "just round it" idiom, not a measurement;
        // scaling it is harmless, and checking it here would only pin an
        // accident. What must hold is that no colour got mangled.
        assert!(two.contains("#1C1B18"), "colours are not lengths");
        assert!(
            two.contains("rgba(0, 0, 0, 0.10)"),
            "alphas are not lengths"
        );
    }

    #[test]
    fn scale_follows_the_narrower_axis_and_stays_sane() {
        assert_eq!(
            scale_for(1280, 664),
            1.0,
            "the design size is 1.0 by definition"
        );
        assert_eq!(
            scale_for(1920, 996),
            1.5,
            "1920x1080 with the HUD taken off"
        );
        // A wide, short output must not scale by width and clip the row.
        assert_eq!(scale_for(2560, 664), 1.0);
        // Degenerate outputs must not produce a degenerate stylesheet.
        assert_eq!(scale_for(0, 0), 1.0);
        assert_eq!(scale_for(320, 200), 0.75, "clamped at the 12px type floor");
    }
}
