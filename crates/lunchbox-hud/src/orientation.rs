//! What the HUD's screen edge implies for its layout.
//!
//! The edge itself is [`lunchbox_api::HudOrientation`], because it travels the
//! wire: it is configured under `[service.hud]` and per entry, resolved by
//! lunchboxd, and pushed to the HUD as `HudOrientationChanged`. This module
//! adds the layout questions only the HUD asks of it.
//!
//! The HUD has always been a horizontal bar pinned to the top (or bottom) of
//! the screen. Issue #171 adds a vertical one down the left edge, for hardware
//! and activities where a portrait strip costs less of the screen than a
//! landscape one — a tall panel, or a game whose own UI lives along the top.
//!
//! The vertical form is specified as "the HUD rotated 90 degrees to the left",
//! and the code takes that literally wherever it can: the same widgets, the
//! same update loop, the same stylesheet, with the flow axis swapped and the
//! order reversed (see [`HudOrientationExt::flow_append`]). Only three things
//! are genuinely different rather than rotated — the activity title, which is
//! drawn sideways by [`crate::rotated_label`]; the wall clock, which becomes an
//! analog face because a digital one is wider than the bar; and the warning
//! banner, whose operator-authored sentence cannot fit in a 48px strip and so
//! moves into a popover.
//!
//! What does *not* rotate is anything pointing at something outside the bar.
//! The page-turn arrows point the way the pages go, so they stay `‹` back and
//! `›` forward in both layouts even though the buttons stack in a column.

use gtk4::glib::object::IsA;
use lunchbox_api::HudOrientation;

/// Parse the `--anchor` flag / `LUNCHBOX_HUD_ANCHOR`.
///
/// Unknown values fall back to `Top` rather than failing: the HUD is the
/// surface a child ends a session from, so a typo must not be able to take it
/// away.
pub fn parse_anchor(value: &str) -> HudOrientation {
    match value {
        "bottom" => HudOrientation::Bottom,
        "left" => HudOrientation::Left,
        _ => HudOrientation::Top,
    }
}

/// The layout questions the HUD asks of its edge.
pub trait HudOrientationExt {
    /// The orientation for every `gtk4::Box` that lays widgets out along the
    /// bar's long axis.
    fn flow(self) -> gtk4::Orientation;

    /// The orientation for a group *within* the bar — a mute button and its
    /// slider, an icon and its readout.
    ///
    /// This is the flow axis too, but the distinction is worth naming: a
    /// vertical group is *not* reversed the way the bar itself is (see
    /// [`Self::flow_append`]). An icon labels the control it sits above, so a
    /// group reads top-to-bottom in source order while the bar around it reads
    /// backwards.
    fn group(self) -> gtk4::Orientation;

    /// Add `child` to `parent` in bar order.
    ///
    /// Rotating the bar a quarter turn to the left maps its **right** end to
    /// the **top** of the screen — so the vertical bar is the horizontal one
    /// read backwards, and the end-session button that lives at the far right
    /// lands at the top, which is where issue #171 asks for it. Prepending as
    /// we go reproduces that without reordering any of the construction code,
    /// so there is exactly one place where the two layouts differ in order and
    /// no chance of the two drifting apart.
    fn flow_append(self, parent: &gtk4::Box, child: &impl IsA<gtk4::Widget>);

    /// The bar's three sections, in the order the *screen* wants them.
    ///
    /// The same reversal as [`Self::flow_append`], as a value rather than as an
    /// insertion: a `CenterBox` is told which child is which and cannot be
    /// filled by appending. `leading` is the end of the bar the mark and the
    /// activity name live at, and on the vertical bar that is the *bottom* of
    /// the screen, so it comes back last.
    fn sections<'a, T>(
        self,
        leading: &'a T,
        centre: &'a T,
        trailing: &'a T,
    ) -> (&'a T, &'a T, &'a T);

    /// Put the bar's three sections into a `CenterBox`, in that order.
    fn flow_sections(
        self,
        parent: &gtk4::CenterBox,
        leading: &impl IsA<gtk4::Widget>,
        centre: &impl IsA<gtk4::Widget>,
        trailing: &impl IsA<gtk4::Widget>,
    );
}

impl HudOrientationExt for HudOrientation {
    fn flow(self) -> gtk4::Orientation {
        if self.is_vertical() {
            gtk4::Orientation::Vertical
        } else {
            gtk4::Orientation::Horizontal
        }
    }

    fn group(self) -> gtk4::Orientation {
        self.flow()
    }

    fn flow_append(self, parent: &gtk4::Box, child: &impl IsA<gtk4::Widget>) {
        use gtk4::prelude::BoxExt;
        if self.is_vertical() {
            parent.prepend(child);
        } else {
            parent.append(child);
        }
    }

    fn sections<'a, T>(
        self,
        leading: &'a T,
        centre: &'a T,
        trailing: &'a T,
    ) -> (&'a T, &'a T, &'a T) {
        if self.is_vertical() {
            (trailing, centre, leading)
        } else {
            (leading, centre, trailing)
        }
    }

    fn flow_sections(
        self,
        parent: &gtk4::CenterBox,
        leading: &impl IsA<gtk4::Widget>,
        centre: &impl IsA<gtk4::Widget>,
        trailing: &impl IsA<gtk4::Widget>,
    ) {
        use gtk4::prelude::Cast;
        let (leading, centre, trailing): (&gtk4::Widget, &gtk4::Widget, &gtk4::Widget) = (
            leading.upcast_ref(),
            centre.upcast_ref(),
            trailing.upcast_ref(),
        );
        let (start, centre, end) = self.sections(leading, centre, trailing);
        parent.set_start_widget(Some(start));
        parent.set_center_widget(Some(centre));
        parent.set_end_widget(Some(end));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_anchor_flag_and_defaults_to_top() {
        assert_eq!(parse_anchor("top"), HudOrientation::Top);
        assert_eq!(parse_anchor("bottom"), HudOrientation::Bottom);
        assert_eq!(parse_anchor("left"), HudOrientation::Left);
        // A typo must not cost the child the button that ends the session.
        assert_eq!(parse_anchor("sideways"), HudOrientation::Top);
        assert_eq!(parse_anchor(""), HudOrientation::Top);
    }

    /// The vertical bar is the horizontal one read backwards, so the section
    /// that leads the bar lands at the *bottom* of the screen — the same
    /// reversal `flow_append` does by prepending, which a `CenterBox` cannot.
    ///
    /// Tested on the ordering rather than on the widgets, because a unit test
    /// has no GTK: constructing one panics with "GTK has not been initialized".
    #[test]
    fn the_vertical_bar_puts_the_leading_section_at_the_bottom() {
        let (mark_end, centre, controls_end) = ("mark", "countdown", "controls");
        assert_eq!(
            HudOrientation::Top.sections(&mark_end, &centre, &controls_end),
            (&mark_end, &centre, &controls_end)
        );
        assert_eq!(
            HudOrientation::Bottom.sections(&mark_end, &centre, &controls_end),
            (&mark_end, &centre, &controls_end)
        );
        // Rotated a quarter turn to the left: the end-session button that sits
        // at the far right of the horizontal bar is at the top of this one, so
        // the mark's end comes last.
        assert_eq!(
            HudOrientation::Left.sections(&mark_end, &centre, &controls_end),
            (&controls_end, &centre, &mark_end)
        );
    }

    #[test]
    fn only_left_lays_out_vertically() {
        assert_eq!(HudOrientation::Left.flow(), gtk4::Orientation::Vertical);
        assert_eq!(HudOrientation::Top.flow(), gtk4::Orientation::Horizontal);
        assert_eq!(HudOrientation::Bottom.flow(), gtk4::Orientation::Horizontal);
    }
}
