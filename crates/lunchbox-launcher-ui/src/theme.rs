//! The Lunchbox look: palette, geometry and the launcher stylesheet.
//!
//! **Nothing here is a number somebody typed twice.** `assets/branding/tokens.json`
//! is the hand-off from the design canvas and the one place a colour, a radius
//! or a type size is decided; `lunchbox-branding` turns it into Rust. The
//! constants below name tokens from that crate, and the stylesheet reaches them
//! through `@name@` placeholders substituted on the way out. Move a value in the
//! token file and it moves here, or the build fails.
//!
//! What is *not* generated is anything the design file does not decide: the
//! shape of the CSS, the two sizes the brief gives in prose rather than tokens,
//! and the handful of places the implementation deliberately departs from the
//! design (each says so, and why).
//!
//! The idea the numbers serve: Lunchbox is a sectioned tin. The field is the
//! enamel, each category is a cream compartment *sunk into* it — which is why
//! the compartment carries inset shadows and never a drop shadow — and time
//! lives on the compartment or the item it applies to, never in a panel of its
//! own.

pub use lunchbox_branding::tokens;

/// Ink, as the components cairo draws with: the only outline colour, and the
/// colour of type.
pub const INK_RGB: (f64, f64, f64) = tokens::COLOR_INK_RGB;
/// Yellow: "you can" — the coin on an earn or a have/need pill.
pub const YELLOW_RGB: (f64, f64, f64) = tokens::COLOR_YELLOW_RGB;
/// Cream: the coin on the deep-teal bank pill, where yellow would lose its ring.
pub const CREAM_RGB: (f64, f64, f64) = tokens::COLOR_CREAM_RGB;

/// The logical width the geometry below is drawn for. A wider output scales
/// the whole field rather than reflowing it: the layout is one row at every
/// size, and scaling is what keeps the ratios.
///
/// From §3 of the brief rather than the token file, which gives the parts of
/// the screen but never the screen.
pub const DESIGN_WIDTH: f64 = 1280.0;
/// The logical height of the *field*: the 720 px screen the design targets,
/// less the HUD bar that sits above this window rather than inside it.
pub const DESIGN_HEIGHT: f64 = 720.0 - tokens::SPACE_HUD_H as f64;

/// Items in a stack before the category spills into a second stack beside it,
/// *as the design hands it down*.
///
/// No longer the number the launcher lays out with. A stack's height is
/// measured against the screen the launcher is actually on
/// (`compartment::rows_that_fit`), because `scale_for` scales by the narrower
/// axis and a screen taller than 16:9 therefore has room the design never
/// budgeted for. This is what that function answers before the window knows
/// its size, and what its arithmetic falls back to if a measurement comes back
/// nonsense — the design's own number, and the right one at 1280×720.
pub const ROWS_PER_STACK: usize = tokens::SPACE_ROWS as usize;
/// Item cell, unscaled.
pub const ITEM_W: i32 = tokens::SPACE_ITEM_W;
/// The cell's height: the design's row height less the row the badge used to
/// occupy under the name.
///
/// A deliberate departure — the badge rides the icon now, so every item is the
/// same height whether or not it has one and the space goes back to the field.
/// Subtracted from the token rather than written as a new number, so a change
/// to the design's row height still carries.
pub const ITEM_H: i32 = tokens::SPACE_ITEM_ROW_H - BADGE_ROW_RECLAIMED;

/// The height the badge no longer needs under the name. See `ITEM_H`.
const BADGE_ROW_RECLAIMED: i32 = 18;

/// The art slot inside an item, and the icon drawn in the middle of it.
///
/// The slot is the one measurement the token file states only in prose — it
/// describes `space.icon` as "App icon size inside a 78 px slot" — so the icon
/// is generated and its slot is not.
pub const ART_SLOT: i32 = 78;
pub const ICON_PX: i32 = tokens::SPACE_ICON;
/// Radius of the ink keyline traced around an icon's silhouette.
pub const KEYLINE: f64 = tokens::STROKE_KEYLINE;

/// Type size of an activity's name, unscaled.
///
/// Needed in Rust as well as in the stylesheet because the leading is a Pango
/// attribute rather than a CSS rule — GTK4 CSS has no `line-height` — and an
/// absolute leading is computed from the size. Both spellings now come from
/// `type.item.size`, so they cannot disagree.
pub const ITEM_FONT_PX: i32 = tokens::TYPE_ITEM_SIZE;

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

/// The launcher stylesheet, resolved against the design tokens and scaled for
/// the output it will be shown on.
///
/// The two passes and the order they run in belong to `lunchbox-branding`,
/// because the HUD's stylesheet needs the same two.
pub fn stylesheet(scale: f64) -> String {
    lunchbox_branding::stylesheet(CSS_TEMPLATE, scale)
}

/// Written at the design size; see `lunchbox_branding::scale_px_literals` for
/// why every length is in `px`. Colours are spelled out rather than named through GTK's own
/// `@define-color`, so that this file and `tokens.json` are diffable.
const CSS_TEMPLATE: &str = r#"
/* ---------------------------------------------------------------- the tin */

/* The field: enamel with a soft sheen across it, so a 1280 px span of flat
   teal doesn't read as a solid colour swatch. */
window.lb-launcher {
    background-image: @color-enamel-sheen@;
    background-color: @color-enamel@;
    color: @color-ink@;
    /* Baloo 2 is shipped with Lunchbox (`assets/fonts`, OFL) and installed by
       `scripts/lunchbox deps install fonts`, because the kiosk is offline by
       default and no distribution packages it. The fallbacks are what the
       launcher looks like if that step has not been run: the layout is
       unchanged and everything is still legible, only the lettering is
       ordinary. If it renders ordinary when you expect otherwise, the cause is
       almost always a stale fontconfig cache rather than a missing file --
       `fc-cache -f` and look again. */
    font-family: @font-display@;
}

/* Vertical margins only. The side margins belong to the *row*, not the field,
   so the scroll viewport reaches the screen edge — which is what lets the edge
   fade end exactly where the enamel sheen reaches its own edge stop, and what
   lets a scrolled compartment run off the screen rather than stopping 40px
   short of it. */
.lb-field {
    padding: @space-field-y@px 0 28px 0;
}

/* The field's side margins, carried by the row so they scroll with it: the
   first compartment starts 40px in, and the last one keeps 40px after it when
   the row is scrolled to the end. */
.lb-field__row {
    padding: 0 @space-field-x@px;
}

/* The edge the row runs out under, at whichever side it continues past.

   Enamel at the outer edge fading to nothing inwards, so a compartment clipped
   by the viewport dissolves into the field rather than ending on a hard
   vertical cut. The colour is `color.enamel-sheen`'s own end stop, pulled out
   of the gradient by `build.rs`, because that is the enamel this sits on top
   of — and matching it is the whole job.

   `rgba(@color-enamel-edge-rgb@, 0)` rather than `transparent`: GTK interpolates a
   gradient through its stop colours, and `transparent` is transparent *black*,
   so the fade would dip grey on its way out. */
.lb-field__fade {
    min-width: 56px;
}

.lb-field__fade--right {
    background-image: linear-gradient(to right,
        rgba(@color-enamel-edge-rgb@, 0), rgba(@color-enamel-edge-rgb@, 1));
}

.lb-field__fade--left {
    background-image: linear-gradient(to left,
        rgba(@color-enamel-edge-rgb@, 0), rgba(@color-enamel-edge-rgb@, 1));
}

/* The yellow chevron chip at the edge the row continues past. */
.lb-more {
    /* Held off the screen edge by hand now that the overlay reaches it. */
    margin: 0 20px;
    background-color: @color-yellow@;
    /* The GTK theme gives a button its own `background-image` gradient, which
       paints straight over a `background-color` and left this chip white. Any
       control the branding recolours has to clear the image as well as set the
       colour — `.lb-item` and `.lb-button` already do. */
    background-image: none;
    border: @stroke-outline@px solid @color-ink@;
    border-radius: @radius-pill@px;
    min-width: 48px;
    min-height: 48px;
    color: @color-ink@;
    box-shadow: @shadow-chip@;
}

/* -------------------------------------------------------- the compartment */

/* Sunk, not stacked: an inner shadow under the top lip, a light line along the
   inner bottom, a faint shade down each inner side, and a dark ring just
   outside the outline. No drop shadow, ever — it is a well, not a card. */
.lb-compartment {
    background-color: @color-compartment@;
    border: @stroke-outline@px solid @color-ink@;
    border-radius: @radius-compartment@px;
    padding: @space-compartment-pad@px;
    box-shadow: @shadow-compartment-sunk@;
}

.lb-compartment__name {
    font-size: @type-category-size@px;
    font-weight: @type-category-weight@;
    color: @color-ink@;
    padding: 0 4px;
}

.lb-compartment__header {
    margin-bottom: 8px;
}

/* The floor: a hairline, then the category's closing time today. */
.lb-compartment__floor {
    border-top: @stroke-hud-rule@px solid @color-putty@;
    margin-top: 8px;
    padding-top: 8px;
}

/* The face on the floor is drawn, not styled, so the only thing the sheet has
   to say about it is its colour — which it reads back with `Widget::color()`.
   Same value as the line it sits in front of; stated rather than inherited,
   because a drawn widget with no colour of its own would take the GTK theme's
   default text colour instead. */
.lb-compartment__clock {
    color: @color-muted@;
}

.lb-compartment__schedule {
    font-family: @font-ui@;
    font-size: @type-footer-size@px;
    font-weight: @type-footer-weight@;
    color: @color-muted@;
}

/* --------------------------------------------------------------- the item */

.lb-item {
    background: none;
    background-color: transparent;
    background-image: none;
    border: @stroke-outline@px solid transparent;
    border-radius: @radius-selection@px;
    padding: 3px;
    box-shadow: none;
    outline: none;
    color: @color-ink@;
    transition: background-color 80ms ease-out, border-color 80ms ease-out;
}

/* The selected cell: filled yellow behind an ink outline.

   Driven by a class the field puts on, not by `:focus`. Two reasons, and the
   second is the one that decided it:

   1. The brief asks that hovering *be* selecting, so there is at most one
      selected item whether the child is using a thumbstick, an arrow key or a
      finger — and none at all until one of them has been used. One class
      expresses that; `:focus, :hover` is two states that can both be true, on
      different items, and neither of which can be switched off while the
      launcher waits to be touched.
   2. `:focus` did not paint here. The item really does hold the focus --
      `grab_focus()` returns true and GTK agrees the widget is focused -- but
      the pseudo-class never matched under the headless compositor. Rather than
      depend on a pseudo-class whose behaviour varies with how the toplevel got
      its keyboard focus, the launcher styles the state it already tracks. */
.lb-item.lb-item--selected {
    background-color: @color-yellow@;
    background-image: none;
    border-color: @color-ink@;
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
    opacity: @opacity-locked@;
}

.lb-item__name {
    font-size: @type-item-size@px;
    font-weight: @type-item-weight@;
    color: @color-ink@;
}

/* -------------------------------------------------------------- the pills */

.lb-badge {
    border-radius: @radius-pill@px;
    border: @stroke-pill@px solid @color-ink@;
    padding: 1px 10px 1px 4px;
    font-size: @type-badge-size@px;
    font-weight: @type-badge-weight@;
}

/* "You can earn here." */
.lb-badge.lb-badge--earn {
    background-color: @color-yellow@;
    color: @color-ink@;
}

/* The one place two yellows meet. A selected cell is filled yellow, so an earn
   pill on it keeps its ink outline and its coin but loses its body — it reads
   as a hole punched in the selection rather than a badge sitting on it. On
   paper the pill stays a pill.

   Only the earn pill needs this: bank is deep teal and need is putty, both of
   which stand off yellow by themselves. */
.lb-item.lb-item--selected .lb-badge.lb-badge--earn {
    background-color: @color-cream@;
}

/* "This much is banked and ready to spend." */
.lb-badge.lb-badge--bank {
    background-color: @color-enamel-deep@;
    color: @color-cream@;
}

/* "Not yet": have against need. */
.lb-badge.lb-badge--need {
    background-color: @color-putty@;
    color: @color-ink@;
}

/* Cooling down. Same shape as the others so the row doesn't jump. */
.lb-badge.lb-badge--wait {
    background-color: @color-putty@;
    color: @color-muted@;
}

/* ------------------------------------------------- everything that is not
   the field: loading, errors, the administrator picker. Branded, but plainly
   so — none of it is what the child is meant to be looking at. */

.lb-message-title {
    font-size: 28px;
    font-weight: 800;
    color: @color-ink@;
}

.lb-message-body {
    font-family: @font-ui@;
    font-size: 16px;
    font-weight: 600;
    color: @color-muted@;
}

.lb-card {
    background-color: @color-compartment@;
    border: @stroke-outline@px solid @color-ink@;
    border-radius: @radius-compartment@px;
    padding: 32px 40px;
    box-shadow: inset 0 10px 0 rgba(0, 0, 0, 0.10),
                0 0 0 3px rgba(0, 0, 0, 0.12);
}

.lb-button {
    background-color: @color-yellow@;
    background-image: none;
    border: @stroke-outline@px solid @color-ink@;
    border-radius: @radius-pill@px;
    padding: 8px 24px;
    font-size: 18px;
    font-weight: 800;
    color: @color-ink@;
    box-shadow: @shadow-chip@;
}

.lb-spinner {
    min-width: 48px;
    min-height: 48px;
    color: @color-ink@;
}

/* Vertical only. The field inside spans the full width, for the same reason
   the child's does: its edge fade has to end where the enamel sheen actually
   reaches its edge stop, which is the screen edge and nowhere else. Inset the
   picker and the fade paints the wrong teal against the window's, and the row
   gets clipped short of the screen with a seam where it stops. */
.admin-picker { padding: 32px 0 0 0; }

/* Which leaves the search entry to hold itself off the edge — at 40px, so it
   lines up with the first compartment rather than floating over its own
   margin. */
.admin-search {
    margin: 0 @space-field-x@px;
    font-size: 20px;
    padding: 10px 14px;
    border-radius: @radius-pill@px;
    border: @stroke-outline@px solid @color-ink@;
    background-color: @color-cream@;
    color: @color-ink@;
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

    /// Both halves of the palette now come from one token, so they cannot
    /// disagree — what can still go wrong is the substitution not happening at
    /// all, leaving a rule GTK silently drops. Looking for the resolved colour
    /// catches that.
    #[test]
    fn the_drawn_colours_reach_the_stylesheet() {
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

    /// Nothing may reach GTK still asking for a token. A stray `@name@` makes
    /// the whole rule invalid, and GTK drops invalid rules without a word — so
    /// the failure would be a missing style in a screenshot rather than an
    /// error anywhere.
    #[test]
    fn no_placeholder_survives() {
        let css = stylesheet(1.0);
        assert!(
            !css.contains('@'),
            "an unresolved token is left in the stylesheet: {}",
            css.split('@').nth(1).unwrap_or_default()
        );
    }

    /// Tokens are substituted before the scaler runs, so a length that came
    /// from the design file scales like any other. Written the other way round,
    /// every token-derived size would stay stuck at its design value.
    #[test]
    fn token_lengths_scale_like_the_rest() {
        let radius: i32 = tokens::CSS
            .iter()
            .find(|(k, _)| *k == "radius-compartment")
            .expect("radius.compartment is a token")
            .1
            .parse()
            .expect("a number");
        assert!(stylesheet(1.0).contains(&format!("border-radius: {radius}px")));
        assert!(stylesheet(2.0).contains(&format!("border-radius: {}px", radius * 2)));
    }

    /// The item name's size is read in Rust as well as styled, because the
    /// leading is a Pango attribute and CSS has no `line-height`. One token
    /// feeds both; this checks the styled half actually arrived.
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
        // Written against the design size rather than the number it works out
        // to. These used to say 664 and 996, which is 720 and 1080 with a 56px
        // HUD taken off — and stopped being true the moment the bar's own token
        // changed (issue #209 took it to 48). The field's height is not this
        // file's to know twice.
        let field = DESIGN_HEIGHT as i32;
        assert_eq!(
            scale_for(DESIGN_WIDTH as i32, field),
            1.0,
            "the design size is 1.0 by definition"
        );
        assert_eq!(
            scale_for((DESIGN_WIDTH * 1.5) as i32, (DESIGN_HEIGHT * 1.5) as i32),
            1.5,
            "a 1.5x output, with the HUD taken off both"
        );
        // A wide, short output must not scale by width and clip the row.
        assert_eq!(scale_for(2560, field), 1.0);
        // Degenerate outputs must not produce a degenerate stylesheet.
        assert_eq!(scale_for(0, 0), 1.0);
        assert_eq!(scale_for(320, 200), 0.75, "clamped at the 12px type floor");
    }
}
