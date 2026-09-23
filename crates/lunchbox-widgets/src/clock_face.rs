//! A little round clock face, drawn flat.
//!
//! Two places want one, for the same reason and at very different sizes:
//!
//! * the **HUD**, in vertical mode, where `HH:MM` does not fit inside a 48 px
//!   bar and a clock read sideways is worse than no clock at all — a round face
//!   is the one form of a clock exactly as wide as it is tall (issue #171);
//! * the **launcher**, on a compartment's floor, ahead of "Until 6:00 PM". That
//!   one is not showing *now*: it points at the hour the category shuts, so a
//!   child who cannot yet read the time can still see where the hand is going
//!   to be (issue #207, and the review on #208 that moved it here).
//!
//! # Size and colour
//!
//! This is drawn rather than styled, so none of it goes through either
//! surface's stylesheet the way a border or a font does. That splits the two
//! knobs a caller needs:
//!
//! * **size** is a number, passed to `new`/`at` and changeable with
//!   [`ClockFace::set_diameter`]. The HUD's bar rescales itself for the current
//!   output and has to re-tell the face; the launcher's field scales once at
//!   build time. Everything below is a fraction of that number, so the face
//!   scales for free.
//! * **colour** is CSS, read back with `Widget::color()`. `color` inherits in
//!   GTK CSS, so a face dropped beside a label is already the label's colour
//!   and a caller that wants otherwise names it in a rule like any other
//!   colour — cream on the HUD's ink bar, muted ink on the launcher's floor.
//!
//! # Appearance
//!
//! A rim and two hands, and nothing else. The first version of this (in the
//! HUD) also drew quarter-hour ticks; the branding concept does not, and the
//! flat ring is what matches the rest of the launcher — see the review on
//! <https://github.com/aarmea/lunchbox/pull/208>. The HUD's own restyle is
//! issue #209.

use chrono::{DateTime, Local};
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use std::cell::Cell;
use std::f64::consts::PI;

/// Diameter of the HUD's face in logical pixels at scale 1.0.
///
/// Sized to the bar's usable width: a 48 px bar with 4 px of padding on each
/// side leaves 40, and the face fills all but a hair of it. It was drawn for
/// the same bar with 6 px of padding, where it fitted exactly; the branding
/// went to 56 px and back to 48 with thinner padding (issue #209), and this is
/// still the right size for it.
pub const HUD_DIAMETER: i32 = 36;

mod imp {
    use super::*;

    pub struct ClockFace {
        /// The time the hands point at, or `None` to follow the wall clock.
        pub time: Cell<Option<DateTime<Local>>>,
        /// Width and height, in logical pixels.
        pub diameter: Cell<i32>,
    }

    impl Default for ClockFace {
        fn default() -> Self {
            Self {
                time: Cell::new(None),
                diameter: Cell::new(HUD_DIAMETER),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ClockFace {
        const NAME: &'static str = "LunchboxClockFace";
        type Type = super::ClockFace;
        type ParentType = gtk4::Widget;
    }

    impl ObjectImpl for ClockFace {}

    impl WidgetImpl for ClockFace {
        fn measure(&self, _orientation: gtk4::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let d = self.diameter.get().max(0);
            (d, d, -1, -1)
        }

        fn snapshot(&self, snapshot: &gtk4::Snapshot) {
            let obj = self.obj();
            let width = f64::from(obj.width());
            let height = f64::from(obj.height());
            let size = width.min(height);
            if size <= 0.0 {
                return;
            }

            let cr = snapshot.append_cairo(&gtk4::graphene::Rect::new(
                0.0,
                0.0,
                width as f32,
                height as f32,
            ));

            // The colour CSS resolved for this widget — see the module docs.
            let color = obj.color();
            cr.set_source_rgba(
                f64::from(color.red()),
                f64::from(color.green()),
                f64::from(color.blue()),
                f64::from(color.alpha()),
            );
            cr.set_line_cap(gtk4::cairo::LineCap::Round);

            let cx = width / 2.0;
            let cy = height / 2.0;
            // Ratio measured off the concept image, where a 16 px face carries
            // a 1.75 px ring. Both bounds are about the two ends this is drawn
            // at: below the floor, on the launcher's floor at 720 p, a
            // fractional stroke greys out into a smudge; above the ceiling, at
            // the size the HUD's bar asks for on a large output, a ring that
            // kept growing with the face would close it into a doughnut.
            let rim = (size / 9.0).clamp(1.5, 3.0);
            // Keep the ring's stroke inside the allocation rather than
            // straddling the edge, which would clip it against the bar.
            let radius = size / 2.0 - rim / 2.0;

            cr.set_line_width(rim);
            cr.arc(cx, cy, radius, 0.0, 2.0 * PI);
            let _ = cr.stroke();

            let now = self.time.get().unwrap_or_else(lunchbox_util::now);
            let (hour_angle, minute_angle) = hand_angles(&now);

            // Angles measured clockwise from 12 o'clock, which is `-cos` on the
            // y axis because GTK's y grows downward.
            let hand = |angle: f64, length: f64, thickness: f64| {
                let (sin, cos) = angle.sin_cos();
                cr.set_line_width(thickness);
                cr.move_to(cx, cy);
                cr.line_to(cx + sin * radius * length, cy - cos * radius * length);
                let _ = cr.stroke();
            };
            // The hour hand is deliberately much shorter and a little heavier
            // than the minute hand, rather than the two being near-twins. At
            // six o'clock — the closing time in half the examples in this
            // repository — they are exactly opposite, and two similar hands
            // there draw one straight line across the face, which reads as a
            // crossed-out circle rather than as a clock.
            hand(hour_angle, 0.45, rim * 1.1);
            hand(minute_angle, 0.80, rim * 0.7);
        }
    }
}

glib::wrapper! {
    pub struct ClockFace(ObjectSubclass<imp::ClockFace>)
        @extends gtk4::Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget;
}

impl ClockFace {
    /// A face that follows the wall clock.
    ///
    /// It reads the time in its own draw function, so it never needs to be told
    /// *what* the time is — but it does have to be told that it changed, with
    /// `queue_draw`, or it keeps the frame it first rendered for the life of
    /// the session.
    pub fn now(diameter: i32) -> Self {
        let face: Self = glib::Object::builder().build();
        face.set_diameter(diameter);
        face
    }

    /// A face whose hands are fixed at one time — a time being talked about
    /// rather than the time it is.
    pub fn at(time: DateTime<Local>, diameter: i32) -> Self {
        let face = Self::now(diameter);
        face.imp().time.set(Some(time));
        face
    }

    /// Resize the face. See the module docs for why this cannot be left to a
    /// stylesheet.
    pub fn set_diameter(&self, diameter: i32) {
        if self.imp().diameter.replace(diameter) != diameter {
            self.queue_resize();
        }
    }
}

/// Where the hour and minute hands point, as angles clockwise from 12 o'clock.
///
/// Split out of the draw function so the arithmetic is testable without a
/// display connection: everything else in `snapshot` needs a realised widget
/// and a cairo surface.
fn hand_angles(time: &DateTime<Local>) -> (f64, f64) {
    use chrono::Timelike;

    let hour = f64::from(time.hour() % 12);
    let minute = f64::from(time.minute());
    // The hour hand advances smoothly with the minutes, so the face never reads
    // a whole hour early at :59.
    ((hour + minute / 60.0) * PI / 6.0, minute * PI / 30.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(hour: u32, minute: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 9, 5, hour, minute, 0).unwrap()
    }

    /// A turn of the hour hand, in the units `hand_angles` returns.
    const TURN: f64 = 2.0 * PI;

    #[test]
    fn afternoon_hours_wrap_onto_the_twelve_hour_face() {
        assert!((hand_angles(&at(0, 0)).0).abs() < 1e-9);
        assert!(
            (hand_angles(&at(12, 0)).0).abs() < 1e-9,
            "noon is not 12 PI"
        );
        // 3 PM and 3 AM point the same way.
        assert!((hand_angles(&at(15, 0)).0 - hand_angles(&at(3, 0)).0).abs() < 1e-9);
        assert!((hand_angles(&at(23, 0)).0 - TURN * 11.0 / 12.0).abs() < 1e-9);
    }

    #[test]
    fn the_hour_hand_creeps_between_the_hours() {
        let (nine, _) = hand_angles(&at(9, 0));
        let (nine_thirty, _) = hand_angles(&at(9, 30));
        let (ten, _) = hand_angles(&at(10, 0));
        assert!(
            nine < nine_thirty && nine_thirty < ten,
            "half past nine sits halfway to ten, not still on the nine"
        );
        assert!((nine_thirty - (nine + ten) / 2.0).abs() < 1e-9);
    }

    #[test]
    fn the_minute_hand_goes_all_the_way_round_in_an_hour() {
        assert!((hand_angles(&at(6, 0)).1).abs() < 1e-9);
        assert!((hand_angles(&at(6, 15)).1 - TURN / 4.0).abs() < 1e-9);
        assert!((hand_angles(&at(6, 45)).1 - TURN * 3.0 / 4.0).abs() < 1e-9);
    }
}
