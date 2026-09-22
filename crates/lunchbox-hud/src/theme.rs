//! The HUD's look: the stylesheet, and the two rules every line in it obeys.
//!
//! **Nothing here is a number or a colour somebody typed twice.**
//! `assets/branding/tokens.json` is the hand-off from the design canvas and the
//! one place either is decided; `lunchbox-branding` turns it into the table this
//! stylesheet reaches through `@name@` placeholders, the way `theme.rs` in
//! `lunchbox-launcher-ui` does for the launcher (issue #209). And every length
//! is written in `px`, so it follows the HUD scale factor — see `css_for_scale`.
//!
//! Three things the token file does not decide, worth knowing before reading a
//! rule and wondering:
//!
//! * **There is exactly one red, and it means a critical time warning.** The
//!   hand-off had none, and spends its loud colour — yellow — on "you can"
//!   rather than on danger, so `color.alert` was added for this bar: running
//!   out of time is the one thing here that has to shout. Picked on contrast
//!   against the near-black bar, 5.99:1 as type and 5.60:1 for ink set on it,
//!   so one colour serves both the countdown and the toast's fill. Nothing else
//!   is red — the end-session "X" is cream like every other control, guarded by
//!   its confirmation (issue #78) rather than by a colour, and offline is putty.
//!   A test holds that line.
//! * **The bar is `color.hud`, not `color.ink`.** They differ by three values of
//!   luminance, which is nothing across a bar this thin; the design file calls
//!   one the bar and the other the outline every shape is drawn with, and the
//!   HUD does not switch between them.
//! * **The controls the brief never mentions** — brightness, the flyouts,
//!   network, display, lock, reset, log out, the confirm prompt, administrator
//!   mode's taskbar, the page turners, the analog clock — keep their positions
//!   and their glyphs and take the palette, the type and the chip. That was the
//!   scope decision this work started from.

/// How loudly the bar says that time is nearly up.
///
/// Three severities are configurable (`[[entries.warnings]]`) and the bar has
/// two ways of showing one, so this is where the three become two — **once**,
/// for both of the things that show it: the toast that appears when a warning
/// fires, and the countdown, which carries it for the rest of the session.
/// They used to decide separately, and disagreed: the countdown read its colour
/// off a pair of hardcoded thresholds (yellow under five minutes, putty under
/// one) which land on the same numbers as the example config's own warnings, so
/// a yellow "1 minute remaining!" toast appeared over a countdown that had
/// already gone putty. Going through here, they cannot.
///
/// `Info` is loud rather than quiet, which is the one judgement in it: an
/// operator who configures a warning at all wants the child to see the bar
/// change, and "5 minutes remaining" is exactly when the countdown used to turn
/// yellow of its own accord. What separates it from `Warn` is what it says.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Urgency {
    /// Yellow: you should know.
    Soon,
    /// Red, and blinking. The one red on the bar; see the note at the top of
    /// this file for where it came from and what it is allowed to mean.
    Now,
}

impl Urgency {
    pub fn from_severity(severity: lunchbox_api::WarningSeverity) -> Self {
        match severity {
            lunchbox_api::WarningSeverity::Info | lunchbox_api::WarningSeverity::Warn => Self::Soon,
            lunchbox_api::WarningSeverity::Critical => Self::Now,
        }
    }

    /// The class on the toast, and on the popover the vertical bar drops it
    /// into.
    pub fn toast_class(self) -> &'static str {
        match self {
            Self::Soon => "warning-warn",
            Self::Now => "warning-critical",
        }
    }

    /// The class on the countdown, which keeps it after the toast has gone.
    pub fn countdown_class(self) -> &'static str {
        match self {
            Self::Soon => "time-warning",
            Self::Now => "time-critical",
        }
    }

    /// Every class either of the two can carry, for the caller that has to
    /// clear the previous one.
    pub const ALL: [Self; 2] = [Self::Soon, Self::Now];
}

/// Build the HUD stylesheet: design tokens resolved, then every px literal
/// scaled by `factor`, so the layer-shell surface stays a constant physical
/// size when lunchboxd drops the compositor scale to 1.0 for an XWayland
/// activity (see the HudScaleChanged event in lunchbox-api).
///
/// Both passes and the order they run in belong to `lunchbox-branding`: a token
/// carries a bare number and the stylesheet spells the unit, so substituting
/// second would leave every token-derived length at its design size.
pub fn css_for_scale(factor: f64) -> String {
    lunchbox_branding::stylesheet(CSS_TEMPLATE, factor)
}

const CSS_TEMPLATE: &str = r#"
        /* ------------------------------------------------------------- the bar

           Ink, opaque, cream type. The bar is the lid of the tin: it is the one
           dark surface in the whole product, and everything on it is either
           cream (what you read), yellow (what you can do) or putty (what is not
           yet). See the note above on the colours this palette does not have.

           Base font size and family for the whole bar. Every size that should
           follow the HUD scale factor has to be written *here*, in px, for
           `scale_px_literals` to counter-scale it; anything left to the GTK
           theme keeps its logical-pixel value and so shrinks on screen by
           1/factor once sway drops to scale 1.0. Setting the size on the root
           means a label that doesn't name its own font-size inherits a scaled
           one instead of falling back to the theme's default (issue #114).

           Baloo 2 is shipped with Lunchbox (`assets/fonts`, OFL) and installed
           by `lunchbox install` and `deps install dev|run`, because the kiosk is
           offline by default and no distribution packages it. The fallbacks are
           what the bar looks like if that step has not been run: the layout is
           unchanged and everything is still legible, only the lettering is
           ordinary. If it renders ordinary when you expect otherwise, the cause
           is almost always a stale fontconfig cache rather than a missing file
           -- `fc-cache -f` and look again. */
        .hud-bar {
            background-color: @color-hud@;
            color: @color-cream@;
            border: none;
            margin: 0;
            padding: 4px @space-compartment-pad@px;
            font-family: @font-display@;
            font-size: @type-hud-size@px;
            font-weight: @type-hud-weight@;
        }

        .app-name {
            font-size: @type-hud-size@px;
            font-weight: @type-hud-weight@;
            color: @color-cream@;
        }

        /* The wordmark, which the bar carries only when no activity is running:
           in an activity the app's own icon and name take this end of the bar.
           The design's own wordmark token is 64px, for the splash it was drawn
           for; on a bar this thin it is the bar's own type, which is what makes it
           read as a label rather than a logo. */
        .hud-wordmark {
            font-size: @type-hud-size@px;
            font-weight: @type-wordmark-weight@;
            color: @color-cream@;
        }

        /* The middle of the bar: the countdown, or the message that has taken
           its place. The margin is what keeps the two side groups off it when
           the bar is full -- it replaces the spacing the bar had while it was
           an ordinary box, and unlike that spacing it is in the stylesheet and
           so follows the HUD scale factor for free. */
        .hud-centre {
            margin: 0 @space-compartment-pad@px;
        }

        .hud-vertical .hud-centre {
            margin: @space-compartment-pad@px 0;
        }

        /* The running activity's icon, keylined. The keyline is cream rather
           than the ink the launcher traces on its cream compartments: it is
           drawn in `Widget::color()` (see lunchbox-widgets), and its whole job
           is to lift the icon off what is behind it — which here is the bar. */
        .hud-app-icon {
            color: @color-cream@;
        }

        /* The vertical bar (issue #171). Everything above applies to it
           unchanged; these are the handful of rules that cannot be the same
           when the long axis is the other one.

           Each still has to be written in px here for `scale_px_literals` to
           counter-scale it -- the vertical layout gets no exemption from the
           rule at the top of this stylesheet. */
        .hud-bar.hud-vertical {
            /* The bar's own padding, turned with it: the 4px that used to be
               above and below the row is now beside the column. */
            padding: @space-compartment-pad@px 4px;
        }

        /* The sliders used to need an axis swap here: written for a
           horizontal bar they name 80px of length and a 4px-thick trough, and
           left alone on a vertical bar they demanded that length *across* it
           -- the surface measured 124px wide instead of 48px. They no longer
           need one, because they are no longer in the bar: both open out of
           their icon as a flyout, which is a popover and so keeps its own
           horizontal axis whichever edge the bar is on (issue #178).

           The lesson the swap taught is still worth keeping for whatever comes
           next, and it has its own trap: state the axis, never the *length*. A
           CSS minimum is a floor GTK takes the maximum of against the widget's
           size request, so a `min-height` restated here silently outranks the
           request -- which is how the #160 reading-session shortening came to
           be inert on the vertical bar, and how #178 ran out of height and
           clipped the page-turn buttons off the bottom. */

        .hud-vertical .network-indicator {
            padding: 2px 0;
        }

        /* The one readout that still has to fit *across* the bar rather than
           along it: at the bar's 18px, "100%" is wider than the space between
           the paddings. The design's footer size is the largest that fits, and
           is still above the 12px floor the branding puts on type. The volume
           and brightness percentages used to be dropped from this bar for the
           same reason; they are in the flyouts now, which have room (#178). */
        .hud-vertical .battery-label {
            font-size: @type-footer-size@px;
        }

        /* ----------------------------------------------------- what is left

           The countdown. Cream, like everything else the child reads: it is the
           bar's primary fact, not a warning, until a warning fires.
           No longer monospace -- the design gives the bar one face, and the
           words beside it ("12:40 left") are set in it. */
        .time-display {
            font-size: @type-hud-size@px;
            font-weight: @type-hud-weight@;
            color: @color-cream@;
        }

        .time-display.time-warning {
            color: @color-yellow@;
        }

        /* The same two appearances as the toast, from the same `Urgency`: the
           countdown is what carries a warning once the toast has gone. The blink
           stays with the red rather than being replaced by it -- a colour says
           "this is different" and a blink says "now", and the last minute of a
           session wants both. See the note at the top of this file. */
        .time-display.time-critical {
            color: @color-alert@;
            animation: blink 1s infinite;
        }

        @keyframes blink {
            50% { opacity: 0.5; }
        }

        /* ------------------------------------------------------------ warnings

           A pill with an ink outline, the same shape as the launcher's badges,
           because it says the same kind of thing: something about time. Two
           appearances -- yellow when you should know, red and blinking when it
           is happening now -- and which one a severity gets is `Urgency`, above,
           because the countdown has to make the same choice and the two must
           not make it separately.

           Both fills carry ink, which is what a pill in this palette always
           carries: `color.alert` was chosen partly so that it still could.

           No outline on the pill, though, which every other pill in the product
           has. An ink outline on an ink bar is a line nobody can see, and it
           cost the pill three pixels of height on a bar that has none to spare.

           The popover the vertical bar drops the message into keeps its
           outline, like the flyouts: it is a surface over the activity rather
           than a shape on the bar, and against unknown content a keyline is the
           whole point. It was taken off once on the strength of a 4x crop that
           appeared to show it missing -- ink on a dark terminal is very nearly
           ink on ink. A pixel scan found it exactly where it was supposed to
           be. Sample the pixels; do not squint at them. */
        .warning-banner {
            background-color: @color-yellow@;
            border-radius: @radius-pill@px;
            padding: 2px 12px;
        }

        .warning-banner.warning-warn {
            background-color: @color-yellow@;
        }

        .warning-banner.warning-critical {
            background-color: @color-alert@;
            animation: blink 1s infinite;
        }

        /* Ink on both fills, which is what a pill in this palette always
           carries; the severity is the fill and the blink, never the letters. */
        .warning-text {
            color: @color-ink@;
            font-size: @type-badge-size@px;
            font-weight: @type-hud-weight@;
        }

        .warning-banner image {
            color: @color-ink@;
        }

        /* The message that no longer fits in the bar. It is a popover rather
           than part of the bar (see `WarningBanner`), so it has to carry the
           pill's own fill -- a popover does not inherit it -- and a width bound
           so an operator's long sentence wraps instead of running off the
           screen. The severity classes land on the popover as well as on the
           banner, so the fill follows the same three rules. */
        .warning-popover > contents {
            background-color: @color-yellow@;
            border: @stroke-pill@px solid @color-ink@;
            border-radius: @radius-selection@px;
            padding: 8px 12px;
        }

        .warning-popover.warning-critical > contents {
            background-color: @color-alert@;
        }

        .warning-popover .warning-text {
            font-size: @type-footer-size@px;
        }

        /* ------------------------------------------------------------ controls

           Cream glyphs on ink, on a chip the size of a finger. The design has
           no opinion on any of these -- brightness, the flyouts, network,
           display, lock, reset, log out, the taskbar, the page turners -- so
           they keep the positions and the glyphs they have and take the
           palette, the type and the chip: 6px corners, a cream wash on hover,
           yellow when a toggle is engaged. */
        image {
            color: @color-cream@;
        }

        /* The analog clock draws itself in whatever colour CSS resolves for
           it (`Widget::color`; see lunchbox-widgets), and it is not an `image`
           node, so without this it inherits the *theme's* default text colour
           -- near-black, and all but invisible against the bar. */
        .analog-clock {
            color: @color-cream@;
        }

        .indicator-button,
        .control-button {
            min-width: 32px;
            min-height: 32px;
            padding: 4px;
            border-radius: @radius-hud-chip@px;
            color: @color-cream@;
        }

        /* The page-turn buttons exist for a finger, on a panel with no
           keyboard, so they get a wider touch target than the 32px an
           indicator gets — a mis-tap here turns no page and reads as the
           activity being broken. Width only: the bar's height is its
           layer-shell exclusive zone, and a taller child pushes the window
           past it, so the HUD grows over the activity while a book is open. */
        .page-button {
            min-width: 44px;
        }

        .indicator-button:hover,
        .control-button:hover {
            background-color: rgba(@color-cream-rgb@, 0.14);
        }

        /* Administrator mode's taskbar (issue #154). Window buttons carry a
           label rather than an icon, so they need room to the sides that the
           square indicator buttons do not. */
        .indicator-button label {
            padding: 0 6px;
        }

        /* Which window the keyboard is talking to. The taskbar is the only
           place that says so — the kiosk hides every border and title bar — so
           it gets the yellow rather than a wash, and ink letters on it. */
        .taskbar-focused {
            background-color: @color-yellow@;
            color: @color-ink@;
        }

        .taskbar-focused label {
            color: @color-ink@;
        }

        /* Automatic brightness is the default, so its toggle stays plain when
           checked (auto on) and lights up only in the *manual* state
           (unchecked, and only when a sensor makes auto an option at all). The
           yellow is the palette's "you are doing this", which is exactly what
           manual means here.

           `.brightness-manual` is the same state shown on the *bar* icon,
           which the update loop sets. The toggle itself moved into the flyout
           with issue #178, and without this the bar would have stopped saying
           who is driving the backlight until someone opened the flyout. */
        .brightness-toggle:not(:checked):not(:disabled),
        .indicator-button.brightness-manual {
            background-color: @color-yellow@;
        }

        .brightness-toggle:not(:checked):not(:disabled) image,
        .indicator-button.brightness-manual image {
            color: @color-ink@;
        }

        /* A muted volume is worth showing on its own toggle the same way, so
           the flyout says which state it is in rather than only the icon
           shape. Checked means muted here, the opposite of the brightness
           toggle above, because muted is the exceptional state. */
        .mute-toggle:checked:not(:disabled) {
            background-color: @color-yellow@;
        }

        .mute-toggle:checked:not(:disabled) image {
            color: @color-ink@;
        }

        /* The GTK theme shades a *checked* toggle button by default. Automatic
           brightness (checked) must look completely plain, so clear that
           shading — keeping only the normal hover feedback. */
        .brightness-toggle:checked {
            background-color: transparent;
            background-image: none;
            box-shadow: none;
        }

        .brightness-toggle:checked:hover {
            background-color: rgba(@color-cream-rgb@, 0.14);
        }

        /* Ending the session is not a warning, so the "X" is cream like every
           other control rather than the red it used to be: the palette has no
           red, and a child's way out of an activity is not an alarm. What
           guards it is the confirmation below, not its colour. */
        .close-button {
            min-width: 32px;
            min-height: 32px;
            padding: 4px;
            border-radius: @radius-hud-chip@px;
            color: @color-cream@;
        }

        .close-button:hover {
            background-color: rgba(@color-cream-rgb@, 0.14);
        }

        .battery-label {
            font-size: @type-hud-size@px;
            font-weight: @type-hud-weight@;
            color: @color-cream@;
        }

        .network-indicator {
            padding: 0 2px;
        }

        /* Online is the enamel the tin is made of; offline is putty, the
           palette's "not yet". */
        .network-indicator.network-online image {
            color: @color-enamel@;
        }

        .network-indicator.network-offline image {
            color: @color-putty@;
        }

        /* -------------------------------------------------------- the flyouts

           A popover is a surface of its own, over the activity rather than on
           the bar, so it is a cream panel with an ink keyline -- a compartment,
           the way the launcher draws one -- and everything in it is ink. Three
           of them: the two sliders and the close confirmation.

           A slider's filled half needs its `border`, `box-shadow` and
           `background-image` cleared as well as its colour set. The GTK theme
           draws its own accent as a 1px border around the highlight, so setting
           only `background-color` left Yaru's orange as a hairline above and
           below the enamel -- six pixels of slider, four of them ours -- on a
           bar with no orange anywhere else. Measured, not squinted at: a
           vertical slice through the filled half read orange, teal, teal, teal,
           teal, orange. The launcher's stylesheet learned the same thing about
           buttons and their `background-image`; this is that trap in other
           clothes.

           Length comes from the widget's size request (`BASE_SLIDER_LENGTH`),
           which is what lets it follow the HUD scale factor. Stating a floor
           here as well would outrank a shorter request -- see the note by the
           `.hud-vertical` rules above. */
        .volume-slider {
            min-width: 0px;
        }

        .volume-slider trough {
            min-height: 4px;
            border-radius: 2px;
            background-color: rgba(@color-ink-rgb@, 0.15);
            background-image: none;
        }

        .volume-slider highlight {
            min-height: 4px;
            border: none;
            box-shadow: none;
            border-radius: 2px;
            background-color: @color-enamel-deep@;
            background-image: none;
        }

        /* 16px and the -8px overhang are what the GTK theme gives the slider
           node on its own, so at factor 1.0 these change nothing — but stating
           them here is what lets the knob grow with the rest of the HUD under
           the counter-scale. The old 12px was below the theme's own minimum, so
           the theme won at factor 1.0 and the knob ended up *smaller* than
           normal at 1.5, making it hard to hit on a touchscreen (issue #114).
           The negative margin has to be restated for the same reason: it is
           what keeps the knob overhanging the trough by a constant amount, and
           it also decides how much of the knob the trough has to accommodate
           (an unscaled -8px against a scaled knob thickens the bar). */
        .volume-slider slider {
            min-width: 16px;
            min-height: 16px;
            margin: -8px;
            border-radius: 50%;
            background-color: @color-ink@;
        }

        .volume-slider:disabled trough {
            background-color: rgba(@color-ink-rgb@, 0.08);
        }

        .volume-slider:disabled highlight {
            background-color: rgba(@color-enamel-deep-rgb@, 0.4);
        }

        .volume-label {
            font-size: @type-caption-size@px;
            color: @color-muted@;
            min-width: 3em;
            text-align: right;
        }

        /* Matches `.volume-slider` -- see the note there. Brightness is yellow
           where volume is teal, because brightness *is* light. */
        .brightness-slider {
            min-width: 0px;
        }

        .brightness-slider trough {
            min-height: 4px;
            border-radius: 2px;
            background-color: rgba(@color-ink-rgb@, 0.15);
            background-image: none;
        }

        .brightness-slider highlight {
            min-height: 4px;
            border: none;
            box-shadow: none;
            border-radius: 2px;
            background-color: @color-yellow@;
            background-image: none;
        }

        /* Matches `.volume-slider slider` — see the note there. */
        .brightness-slider slider {
            min-width: 16px;
            min-height: 16px;
            margin: -8px;
            border-radius: 50%;
            background-color: @color-ink@;
        }

        .brightness-slider:disabled trough {
            background-color: rgba(@color-ink-rgb@, 0.08);
        }

        .brightness-slider:disabled highlight {
            background-color: rgba(@color-yellow-rgb@, 0.4);
        }

        .brightness-label {
            font-size: @type-caption-size@px;
            color: @color-muted@;
            min-width: 3em;
            text-align: right;
        }

        .clock-label {
            font-size: @type-hud-size@px;
            font-weight: @type-hud-weight@;
            color: @color-cream@;
        }

        /* Debug builds only, and still above the branding's 12px floor on
           type: nothing on this bar is allowed to be unreadable, including the
           thing that says the clock is lying. */
        .mock-time-indicator {
            font-size: @type-minimum-size@px;
            font-weight: @type-hud-weight@;
            color: @color-yellow@;
            margin-left: 4px;
        }

        /* Cream panel, ink keyline, ink type -- a compartment lifted off the
           bar. Colours are stated here rather than left to theme variables so
           the prompt keeps its contrast whatever GTK theme is installed, and
           never lets the bright activity behind it bleed through.

           No arrow on any of the three popovers -- `set_has_arrow(false)`,
           because the wedge is a widget rather than a rule and CSS can only
           recolour it, not remove the space it takes. It never earned its keep
           here: a popover on this bar drops from the control that opened it and
           has nowhere else it could have come from, and the wedge is the one
           part of the panel a keyline cannot follow, so it read as a notch
           taken out of the outline. */
        .confirm-close-popover > contents {
            background-color: @color-cream@;
            color: @color-ink@;
            border: @stroke-outline@px solid @color-ink@;
            border-radius: @radius-selection@px;
            padding: 14px;
            /* The popover is its own surface, so state the base font size here
               too rather than relying on inheriting the bar's (issue #114):
               without it the Cancel / End labels keep the theme's unscaled
               size while the box around them grows. */
            font-size: @type-footer-size@px;
        }

        /* The pop-out volume / brightness controls (issue #178). Same cream
           panel as the prompt above and for the same reasons: a popover does
           not inherit the bar's background, the activity behind it must not
           bleed through, and the base font size has to be stated here or the
           readout inside falls back to the theme's unscaled default (#114). */
        .slider-popover > contents {
            background-color: @color-cream@;
            color: @color-ink@;
            border: @stroke-outline@px solid @color-ink@;
            border-radius: @radius-selection@px;
            padding: 14px;
            font-size: @type-footer-size@px;
        }

        .slider-popover image {
            color: @color-ink@;
        }

        .confirm-close-message {
            color: @color-ink@;
            font-size: @type-hud-size@px;
            font-weight: @type-hud-weight@;
        }

        /* Theme buttons paint a gradient via background-image, which a bare
           background-color won't override, so clear it and set explicit fills.
           Cancel is putty -- the palette's "nothing happens" -- and ending the
           activity is the deep teal of a decision taken, with cream on it.
           Neither is red, for the reason at the top of this file. */
        .confirm-close-popover button {
            min-height: 32px;
            padding: 6px 14px;
            border-radius: @radius-hud-chip@px;
            border: @stroke-pill@px solid @color-ink@;
            background-image: none;
            color: @color-ink@;
            background-color: @color-putty@;
            /* State the font-size on the button node itself, not just on
               `> contents`. #114 set the base size on the popover surface
               expecting the Cancel / End labels to inherit it, but the GTK
               theme sets an explicit `font-size` on `button`, which is more
               specific than the inherited `> contents` value and wins the
               cascade — so the labels kept the theme's logical-pixel size and
               rendered 1/factor too small under the counter-scale, while the
               button box around them (min-height/padding, stated here in px)
               grew. Restating it here, at higher specificity than the theme's
               bare `button`, is what lets the label follow the HUD factor. */
            font-size: @type-footer-size@px;
        }

        .confirm-close-popover button:hover {
            background-color: @color-compartment@;
        }

        .confirm-close-popover button.destructive-action {
            color: @color-cream@;
            background-color: @color-enamel-deep@;
        }

        /* Hover goes *darker*, not lighter: cream on `color.enamel` is 2.41:1,
           on the one button where misreading the label costs a child their
           session. Ink is 16.35:1. */
        .confirm-close-popover button.destructive-action:hover {
            background-color: @color-ink@;
        }
    "#;

#[cfg(test)]
mod tests {
    use super::*;

    /// One rule's body, read out of the stylesheet as GTK gets it: tokens
    /// resolved, lengths at their design size. Reading the template instead
    /// would see `@type-hud-size@px` where a test wants a number.
    fn rule(selector: &str) -> String {
        let css = css_for_scale(1.0);
        css.split_once(selector)
            .and_then(|(_, rest)| rest.split_once('}'))
            .map(|(block, _)| block.to_string())
            .unwrap_or_else(|| panic!("{selector} rule missing from the stylesheet"))
    }

    /// Issue #114: the bar and the confirm popover must each state a base
    /// `font-size`. A label that inherits the *theme's* default instead keeps
    /// its logical-pixel size and so renders 1/factor too small once lunchboxd
    /// drops the compositor scale for an XWayland activity — the bug the
    /// warning banner text showed.
    #[test]
    fn text_roots_declare_a_scalable_font_size() {
        for root in [
            ".hud-bar {",
            ".confirm-close-popover > contents {",
            ".slider-popover > contents {",
        ] {
            assert!(
                rule(root).contains("font-size:"),
                "{root} must set a font-size so labels don't fall back to the theme default"
            );
        }
        // ...and the bar's own size has to follow the factor.
        let base = lunchbox_branding::tokens::TYPE_HUD_SIZE;
        assert!(
            rule(".hud-bar {").contains(&format!("font-size: {base}px")),
            "the bar is set in the design's own HUD type size"
        );
        assert!(
            css_for_scale(2.0).contains(&format!("font-size: {}px", base * 2)),
            "and that size doubles with the factor"
        );
    }

    /// The bar names Baloo 2, and at the weight the design gives it. Issue #209:
    /// the HUD used to name no family at all, so it rendered in whatever sans
    /// the GTK theme picked while the launcher under it was in the display face.
    #[test]
    fn the_bar_is_set_in_the_display_face() {
        let bar = rule(".hud-bar {");
        assert!(bar.contains("Baloo 2"), "{bar}");
        assert!(
            bar.contains(&format!(
                "font-weight: {}",
                lunchbox_branding::tokens::TYPE_HUD_WEIGHT
            )),
            "{bar}"
        );
    }

    /// Nothing may reach GTK still asking for a token. A stray `@name@` makes
    /// the whole rule invalid, and GTK drops invalid rules without a word — so
    /// the failure would be a missing style in a screenshot rather than an
    /// error anywhere. The blink animation's own `@keyframes` is the one `@`
    /// that belongs here.
    #[test]
    fn no_placeholder_survives() {
        let css = css_for_scale(1.0);
        for fragment in css.split('@').skip(1) {
            assert!(
                fragment.starts_with("keyframes"),
                "an unresolved token is left in the stylesheet: @{}",
                fragment.split_whitespace().next().unwrap_or_default()
            );
        }
    }

    /// Every appearance an `Urgency` can ask for has a rule in the stylesheet.
    ///
    /// This is what makes the toast and the countdown agree *in practice* as
    /// well as by construction: they go through one mapping, and a class either
    /// of them names has to exist here or GTK silently renders it unstyled —
    /// the toast would come out cream and the countdown would stop saying
    /// anything at all.
    #[test]
    fn both_halves_of_an_urgency_are_styled() {
        let css = css_for_scale(1.0);
        for urgency in Urgency::ALL {
            for class in [urgency.toast_class(), urgency.countdown_class()] {
                assert!(
                    css.contains(&format!(".{class} ")) || css.contains(&format!(".{class} {{")),
                    "{urgency:?} asks for `.{class}`, which the stylesheet does not define"
                );
            }
        }
    }

    /// The three configurable severities become two appearances here and
    /// nowhere else. A second mapping is how the countdown and the toast came
    /// to disagree in the first place, so this pins the one that is left.
    #[test]
    fn a_severity_has_exactly_one_appearance() {
        use lunchbox_api::WarningSeverity::{Critical, Info, Warn};
        assert_eq!(Urgency::from_severity(Info), Urgency::Soon);
        assert_eq!(Urgency::from_severity(Warn), Urgency::Soon);
        assert_eq!(Urgency::from_severity(Critical), Urgency::Now);
        // And the stylesheet has no leftover rule for the severity that no
        // longer has an appearance of its own.
        assert!(
            !css_for_scale(1.0).contains("warning-info"),
            "`warning-info` is styled but nothing can ask for it any more"
        );
    }

    /// The stylesheet's rules, as `(selector, body)`, with comments stripped —
    /// a comment in this file is as long as the rule it explains and full of
    /// the words the rules use.
    fn rules(css: &str) -> Vec<(String, String)> {
        let mut plain = String::with_capacity(css.len());
        let mut rest = css;
        while let Some(start) = rest.find("/*") {
            plain.push_str(&rest[..start]);
            rest = match rest[start..].find("*/") {
                Some(end) => &rest[start + end + 2..],
                None => "",
            };
        }
        plain.push_str(rest);
        plain
            .split('}')
            .filter_map(|block| block.split_once('{'))
            .map(|(selector, body)| {
                (
                    selector.split_whitespace().collect::<Vec<_>>().join(" "),
                    body.to_string(),
                )
            })
            .collect()
    }

    /// Red means one thing on this bar: a critical time warning.
    ///
    /// The palette had no red at all, and the three things that used to carry
    /// Nord's — the end-session "X", the offline indicator, a critical warning —
    /// are the ones a reviewer will reach for first. Only the last of them may
    /// have it, and only through `color.alert`; the other two say what they
    /// mean without shouting, and a change that makes either red is a change to
    /// what red means here.
    #[test]
    fn red_means_a_critical_warning_and_nothing_else() {
        let css = css_for_scale(1.0);
        let alert = lunchbox_branding::tokens::COLOR_ALERT;
        let wearing: Vec<String> = rules(&css)
            .into_iter()
            .filter(|(_, body)| body.contains(alert))
            .map(|(selector, _)| selector)
            .collect();
        assert_eq!(
            wearing,
            vec![
                ".time-display.time-critical",
                ".warning-banner.warning-critical",
                ".warning-popover.warning-critical > contents",
            ],
            "{alert} is worn by something other than a critical warning"
        );

        // And nothing has quietly brought back one of the reds this palette
        // replaced.
        let lower = css.to_ascii_lowercase();
        for red in ["#ff6b6b", "#bf616a", "#d08770", "255, 107, 107"] {
            assert!(!lower.contains(red), "{red} is back in the HUD stylesheet");
        }
    }

    /// Every colour on the bar comes from the token file. Catches the other
    /// direction from `no_placeholder_survives`: a hex value typed straight
    /// into a rule resolves fine and looks fine, and is exactly the second copy
    /// this arrangement exists to prevent.
    #[test]
    fn no_colour_is_written_by_hand() {
        let from_tokens: Vec<String> = lunchbox_branding::tokens::CSS
            .iter()
            .filter(|(_, v)| v.starts_with('#'))
            .map(|(_, v)| v.to_ascii_lowercase())
            .collect();
        let css = css_for_scale(1.0).to_ascii_lowercase();
        for (i, _) in css.match_indices('#') {
            let hex: String = css[i..].chars().take(7).collect();
            // `#114` in a comment naming an issue is not a colour, and this
            // file is full of them.
            if hex.len() < 7 || !hex[1..].bytes().all(|b| b.is_ascii_hexdigit()) {
                continue;
            }
            assert!(
                from_tokens.contains(&hex),
                "{hex} is in the stylesheet but not in tokens.json"
            );
        }
    }

    /// Issue #114 follow-up: the confirm popover's Cancel / End buttons must
    /// state their own `font-size`, not rely on inheriting the popover surface's
    /// (`> contents`). The GTK theme sets an explicit `font-size` on `button`,
    /// which is more specific than the inherited value and wins the cascade — so
    /// without a rule of its own the button label kept the theme's logical-pixel
    /// size and rendered 1/factor too small under the counter-scale, even though
    /// the box around it grew.
    #[test]
    fn confirm_popover_button_declares_its_own_font_size() {
        let selector = ".confirm-close-popover button {";
        assert!(
            rule(selector).contains("font-size:"),
            "{selector} must set a font-size so the label scales instead of \
             inheriting the theme's unscaled button font"
        );
    }

    /// Issue #114: the slider knob has to be at least as big as the size the
    /// GTK theme would pick on its own (16px), or the theme wins the cascade at
    /// factor 1.0 and the counter-scaled value comes out smaller than the
    /// un-scaled knob — a shrinking touch target.
    #[test]
    fn slider_knob_is_scaled_from_at_least_the_theme_size() {
        for slider in [".volume-slider slider {", ".brightness-slider slider {"] {
            let block = rule(slider);
            for dim in ["min-width", "min-height"] {
                let value: i32 = block
                    .split_once(&format!("{dim}:"))
                    .and_then(|(_, rest)| rest.split_once("px"))
                    .and_then(|(value, _)| value.trim().parse().ok())
                    .unwrap_or_else(|| panic!("{slider} must set {dim} in px"));
                assert!(
                    value >= 16,
                    "{slider} {dim} is {value}px; below the theme's own 16px it does not scale"
                );
            }
        }
    }

    /// Issue #178: the flyout is a text root of its own, so like the bar and
    /// the confirm prompt it has to state a `font-size` — its percentage
    /// readout would otherwise keep the theme's logical-pixel size and render
    /// 1/factor too small under the counter-scale (issue #114's rule).
    #[test]
    fn the_slider_flyout_declares_a_scalable_font_size() {
        let selector = ".slider-popover > contents {";
        assert!(
            rule(selector).contains("font-size:"),
            "{selector} must set a font-size so the readout does not fall back \
             to the theme default"
        );
    }

    /// Issue #178: neither slider rule may state a *length* floor.
    ///
    /// A CSS minimum is a floor GTK takes the maximum of against the widget's
    /// size request, so a `min-width` here outranks a shorter request — which
    /// is exactly how the vertical bar's swapped rule silently cancelled the
    /// #160 reading-session shortening and left the page-turn buttons clipped
    /// off the bottom of the bar. The length is `BASE_SLIDER_LENGTH`, applied
    /// as a request so it can follow the HUD scale factor.
    #[test]
    fn slider_rules_leave_their_length_to_the_size_request() {
        for selector in [".volume-slider {", ".brightness-slider {"] {
            let value: i32 = rule(selector)
                .split_once("min-width:")
                .and_then(|(_, rest)| rest.split_once("px"))
                .and_then(|(value, _)| value.trim().parse().ok())
                .unwrap_or_else(|| panic!("{selector} must state min-width in px"));
            assert_eq!(
                value, 0,
                "{selector} min-width is {value}px, which outranks the slider's \
                 own size request"
            );
        }
    }

    /// The sliders are out of the bar, so nothing in the stylesheet should
    /// still be turning them for the vertical layout. A leftover rule here
    /// would apply to the flyout — a popover is a descendant of the bar icon
    /// it is parented to, so `.hud-vertical` still matches inside it — and
    /// would zero the width of a slider that is horizontal in both layouts.
    #[test]
    fn the_vertical_layout_no_longer_turns_the_sliders() {
        for dead in [
            ".hud-vertical .volume-slider",
            ".hud-vertical .brightness-slider",
            ".hud-vertical .volume-control",
            ".hud-vertical .brightness-control",
        ] {
            // Only selectors count; the explanatory comment above them names
            // the rules deliberately, and naming them is the point.
            let stylesheet: String = CSS_TEMPLATE
                .lines()
                .filter(|line| !line.trim_start().starts_with(['/', '*', '-']))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                !stylesheet.contains(dead),
                "{dead} is still in the stylesheet; the sliders left the bar in \
                 issue #178 and a rule that turns them now hits the flyout"
            );
        }
    }
}
