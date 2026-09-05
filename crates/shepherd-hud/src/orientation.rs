//! Which screen edge the HUD occupies, and what that implies for its layout.
//!
//! The HUD has always been a horizontal bar pinned to the top (or bottom) of
//! the screen. Issue #171 adds a vertical one down the left edge, for hardware
//! and activities where a portrait strip costs less of the screen than a
//! landscape one — a tall panel, or a game whose own UI lives along the top.
//!
//! The vertical form is specified as "the HUD rotated 90 degrees to the left",
//! and the code takes that literally wherever it can: the same widgets, the
//! same update loop, the same stylesheet, with the flow axis swapped and the
//! order reversed (see [`HudOrientation::flow_append`]). Only three things are
//! genuinely different rather than rotated — the activity title, which is drawn
//! sideways by [`crate::rotated_label`]; the wall clock, which becomes an
//! analog face because a digital one is wider than the bar; and the warning
//! banner, whose operator-authored sentence cannot fit in a 48px strip and so
//! moves into a popover.

use gtk4::glib::object::IsA;

/// The screen edge the HUD is anchored to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HudOrientation {
    /// A horizontal bar along the top edge. The default, and what every
    /// device shipped before issue #171 uses.
    #[default]
    Top,
    /// A horizontal bar along the bottom edge.
    Bottom,
    /// A vertical bar down the left edge (issue #171).
    Left,
}

impl HudOrientation {
    /// Parse the `--anchor` flag. Unknown values fall back to `Top` rather
    /// than failing: the HUD is the surface a child ends a session from, so a
    /// typo in a config file must not be able to take it away.
    pub fn parse(value: &str) -> Self {
        match value {
            "bottom" => Self::Bottom,
            "left" => Self::Left,
            _ => Self::Top,
        }
    }

    /// Whether the bar runs down the screen rather than across it.
    pub fn is_vertical(self) -> bool {
        matches!(self, Self::Left)
    }

    /// The orientation for every `gtk4::Box` that lays widgets out along the
    /// bar's long axis.
    pub fn flow(self) -> gtk4::Orientation {
        if self.is_vertical() {
            gtk4::Orientation::Vertical
        } else {
            gtk4::Orientation::Horizontal
        }
    }

    /// The orientation for a group *within* the bar — a mute button and its
    /// slider, an icon and its readout.
    ///
    /// This is the flow axis too, but the distinction is worth naming: a
    /// vertical group is *not* reversed the way the bar itself is (see
    /// [`Self::flow_append`]). An icon labels the control it sits above, so a
    /// group reads top-to-bottom in source order while the bar around it reads
    /// backwards.
    pub fn group(self) -> gtk4::Orientation {
        self.flow()
    }

    /// Add `child` to `parent` in bar order.
    ///
    /// Rotating the bar a quarter turn to the left maps its **right** end to
    /// the **top** of the screen — so the vertical bar is the horizontal one
    /// read backwards, and the end-session button that lives at the far right
    /// lands at the top, which is where issue #171 asks for it. Prepending as
    /// we go reproduces that without reordering any of the construction code,
    /// so there is exactly one place where the two layouts differ in order and
    /// no chance of the two drifting apart.
    pub fn flow_append(self, parent: &gtk4::Box, child: &impl IsA<gtk4::Widget>) {
        use gtk4::prelude::BoxExt;
        if self.is_vertical() {
            parent.prepend(child);
        } else {
            parent.append(child);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_anchor_flag_and_defaults_to_top() {
        assert_eq!(HudOrientation::parse("top"), HudOrientation::Top);
        assert_eq!(HudOrientation::parse("bottom"), HudOrientation::Bottom);
        assert_eq!(HudOrientation::parse("left"), HudOrientation::Left);
        // A typo must not cost the child the button that ends the session.
        assert_eq!(HudOrientation::parse("sideways"), HudOrientation::Top);
        assert_eq!(HudOrientation::parse(""), HudOrientation::Top);
    }

    #[test]
    fn only_left_is_vertical() {
        assert!(HudOrientation::Left.is_vertical());
        assert!(!HudOrientation::Top.is_vertical());
        assert!(!HudOrientation::Bottom.is_vertical());
    }
}
