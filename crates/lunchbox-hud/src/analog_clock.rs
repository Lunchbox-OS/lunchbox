//! An analog wall clock face, for the vertical HUD (issue #171).
//!
//! The horizontal bar shows the wall clock as `HH:MM`. That text is about
//! 45 logical pixels wide, which does not fit inside a 48px-wide vertical bar
//! once the bar's own padding is taken out — and unlike the activity title, a
//! clock read sideways is worse than no clock at all. A round face is the one
//! form of a clock that is exactly as wide as it is tall, so it is what the
//! vertical bar uses.
//!
//! # Scale factor
//!
//! This is drawn, not styled, so none of it goes through `CSS_TEMPLATE` and
//! `scale_px_literals` never sees it. Per the rule in the crate README, that
//! means the size has to be set explicitly from the current HUD scale factor —
//! see `set_diameter`, called from the 500ms timer in `app.rs` alongside the
//! icon `set_pixel_size` and slider width calls. Everything the draw function
//! does is expressed as a fraction of the allocated size, so the face itself
//! then scales for free.

use gtk4::prelude::*;
use std::f64::consts::PI;

/// Diameter of the clock face in logical pixels at scale 1.0.
///
/// Sized to the bar's usable width: a 48px bar with 6px of padding on each
/// side leaves 36px, and the face fills it.
pub const BASE_CLOCK_DIAMETER: i32 = 36;

/// A round clock face showing `hour`/`minute`, drawn in the widget's CSS
/// colour so it matches the icons on either side of it.
pub fn build() -> gtk4::DrawingArea {
    let area = gtk4::DrawingArea::builder()
        .content_width(BASE_CLOCK_DIAMETER)
        .content_height(BASE_CLOCK_DIAMETER)
        .build();
    area.add_css_class("analog-clock");

    area.set_draw_func(|area, cr, width, height| {
        // `Widget::color()` is the colour CSS resolved for this widget, so the
        // face follows the same `--text-primary` the icons use and needs no
        // palette of its own.
        let color = area.color();
        cr.set_source_rgba(
            color.red() as f64,
            color.green() as f64,
            color.blue() as f64,
            color.alpha() as f64,
        );

        let size = f64::from(width.min(height));
        if size <= 0.0 {
            return;
        }
        let cx = f64::from(width) / 2.0;
        let cy = f64::from(height) / 2.0;
        // Keep the rim's stroke inside the allocation rather than straddling
        // the edge, which would clip it against the bar.
        let rim = size / 16.0;
        let radius = size / 2.0 - rim / 2.0;

        cr.set_line_width(rim);
        cr.arc(cx, cy, radius, 0.0, 2.0 * PI);
        let _ = cr.stroke();

        // Quarter-hour ticks. Four is enough to read the face at this size;
        // twelve would merge into a grey ring by the time it is 36px across.
        cr.set_line_width(rim * 0.8);
        for quarter in 0..4 {
            let angle = f64::from(quarter) * PI / 2.0;
            let (sin, cos) = angle.sin_cos();
            cr.move_to(cx + sin * radius * 0.78, cy - cos * radius * 0.78);
            cr.line_to(cx + sin * radius * 0.95, cy - cos * radius * 0.95);
        }
        let _ = cr.stroke();

        let now = lunchbox_util::now();
        let (hour, minute) = clock_hands(&now);

        // Angles measured clockwise from 12 o'clock, which is `-cos` on the y
        // axis because GTK's y grows downward.
        let hand = |angle: f64, length: f64, thickness: f64| {
            let (sin, cos) = angle.sin_cos();
            cr.set_line_width(thickness);
            cr.set_line_cap(gtk4::cairo::LineCap::Round);
            cr.move_to(cx, cy);
            cr.line_to(cx + sin * radius * length, cy - cos * radius * length);
            let _ = cr.stroke();
        };

        // The hour hand advances smoothly with the minutes, so the face never
        // reads a whole hour early at :59.
        let hour_angle = (f64::from(hour) + f64::from(minute) / 60.0) * PI / 6.0;
        let minute_angle = f64::from(minute) * PI / 30.0;
        hand(hour_angle, 0.52, rim);
        hand(minute_angle, 0.80, rim * 0.75);
    });

    area
}

/// Resize the face for the current HUD scale factor. See the module docs for
/// why this cannot be left to the stylesheet.
pub fn set_diameter(area: &gtk4::DrawingArea, diameter: i32) {
    area.set_content_width(diameter);
    area.set_content_height(diameter);
}

/// The hour (0-11) and minute the hands should point at.
///
/// Split out from the draw function so the wrap-around behaviour is testable
/// without a display connection: everything else in `build` needs a real GTK
/// widget and a cairo surface.
fn clock_hands(time: &chrono::DateTime<chrono::Local>) -> (u32, u32) {
    use chrono::Timelike;
    (time.hour() % 12, time.minute())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(hour: u32, minute: u32) -> chrono::DateTime<chrono::Local> {
        chrono::Local
            .with_ymd_and_hms(2026, 9, 5, hour, minute, 0)
            .unwrap()
    }

    #[test]
    fn afternoon_hours_wrap_onto_the_twelve_hour_face() {
        assert_eq!(clock_hands(&at(0, 0)), (0, 0));
        assert_eq!(clock_hands(&at(9, 30)), (9, 30));
        assert_eq!(clock_hands(&at(12, 0)), (0, 0));
        assert_eq!(clock_hands(&at(15, 45)), (3, 45));
        assert_eq!(clock_hands(&at(23, 59)), (11, 59));
    }
}
