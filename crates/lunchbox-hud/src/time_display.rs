//! Time display widget
//!
//! Shows elapsed time, remaining time, or countdown.

use gtk4::glib;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use std::cell::{Cell, RefCell};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct TimeDisplay {
        pub label: RefCell<Option<gtk4::Label>>,
        pub total_secs: RefCell<Option<u64>>,
        pub remaining_secs: RefCell<Option<u64>>,
        /// Render at most three characters (`90m`, `1h`) instead of
        /// `HH:MM:SS`. Set for the vertical HUD, where the bar is 48px wide
        /// and a clock-style readout does not fit (issue #171).
        pub compact: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TimeDisplay {
        const NAME: &'static str = "LunchboxTimeDisplay";
        type Type = super::TimeDisplay;
        type ParentType = gtk4::Box;
    }

    impl ObjectImpl for TimeDisplay {
        fn constructed(&self) {
            self.parent_constructed();

            let obj = self.obj();
            obj.set_orientation(gtk4::Orientation::Horizontal);
            obj.set_spacing(4);

            // Time label. There is deliberately no icon beside it: a countdown
            // is self-describing, and the clock glyph that used to sit here
            // was the one element of the bar that said nothing the numbers did
            // not already say (issue #178).
            let label = gtk4::Label::new(Some("--:--"));
            label.add_css_class("time-display");
            obj.append(&label);

            *self.label.borrow_mut() = Some(label);
        }
    }

    impl WidgetImpl for TimeDisplay {}
    impl BoxImpl for TimeDisplay {}
}

glib::wrapper! {
    pub struct TimeDisplay(ObjectSubclass<imp::TimeDisplay>)
        @extends gtk4::Box, gtk4::Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget, gtk4::Orientable;
}

impl TimeDisplay {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    /// Lay the readout out for the vertical HUD: the three-character duration
    /// format, and centred across the bar.
    ///
    /// Both are the same problem — the readout has 48px to fit *across* rather
    /// than an open bar to sit along — so they are one call.
    ///
    /// The centring has to be asked for. A vertical box allocates every child
    /// the full width of the bar, and this widget's own label is packed at the
    /// start of it, so the default `Fill` left the countdown hard against the
    /// left edge while the clock face and the activity title above it were
    /// centred. `Fill` is restored for the horizontal bar, where the widget is
    /// allocated its natural width and the alignment makes no difference
    /// either way.
    pub fn set_compact(&self, compact: bool) {
        let imp = self.imp();
        imp.compact.set(compact);
        self.set_halign(if compact {
            gtk4::Align::Center
        } else {
            gtk4::Align::Fill
        });
        self.update_display();
    }

    /// Set the time limit in seconds
    pub fn set_time_limit(&self, total_secs: Option<u64>) {
        let imp = self.imp();
        *imp.total_secs.borrow_mut() = total_secs;
        self.update_display();
    }

    /// Set the remaining time in seconds
    pub fn set_remaining(&self, remaining_secs: Option<u64>) {
        let imp = self.imp();
        *imp.remaining_secs.borrow_mut() = remaining_secs;
        self.update_display();
    }

    /// Update the display based on current state
    fn update_display(&self) {
        let imp = self.imp();

        if let Some(label) = imp.label.borrow().as_ref() {
            let remaining = *imp.remaining_secs.borrow();

            let compact = imp.compact.get();
            let text = match remaining {
                Some(secs) if compact => format_compact(secs),
                Some(secs) => format_duration(secs),
                None if compact => "--".to_string(),
                None => "--:--".to_string(),
            };

            label.set_text(&text);

            // Update styling based on remaining time
            label.remove_css_class("time-warning");
            label.remove_css_class("time-critical");

            if let Some(secs) = remaining {
                if secs <= 60 {
                    label.add_css_class("time-critical");
                } else if secs <= 300 {
                    label.add_css_class("time-warning");
                }
            }
        }
    }
}

impl Default for TimeDisplay {
    fn default() -> Self {
        Self::new()
    }
}

/// Format a duration in seconds as HH:MM:SS or MM:SS
fn format_duration(secs: u64) -> String {
    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;

    if hours > 0 {
        format!("{:02}:{:02}:{:02}", hours, minutes, seconds)
    } else {
        format!("{:02}:{:02}", minutes, seconds)
    }
}

/// Format a duration in at most three characters, for the vertical HUD
/// (issue #171): one unit and one number, never more than two digits of it.
///
/// The 48px-wide bar has room for about three characters at the bar's font
/// size, so the format is chosen to fit that budget rather than to be precise:
/// seconds below a minute, then whole minutes for as long as they fit two
/// digits, then whole hours.
///
/// The cost is the 100-119 minute band, which reads `1h` and so hides up to
/// 59 minutes. That is deliberate and was chosen over letting the readout grow
/// to `119m`: a bar this narrow cannot afford a fourth character, and the
/// child has the hour to notice the number changing. The horizontal HUD keeps
/// its precise `H:MM:SS` and is unaffected.
fn format_compact(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 100 * 60 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h", secs / 3600)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_format_never_exceeds_three_characters() {
        // Whatever else changes, the width budget is the point of this format:
        // a fourth character does not fit the 48px bar. Walk a full day.
        for secs in (0..=24 * 3600).step_by(7) {
            let text = format_compact(secs);
            assert!(
                text.chars().count() <= 3,
                "format_compact({secs}) = {text:?} is wider than the bar"
            );
        }
    }

    #[test]
    fn compact_format_switches_units_at_the_agreed_cutoffs() {
        assert_eq!(format_compact(0), "0s");
        assert_eq!(format_compact(59), "59s");
        // A minute is the first value shown in minutes, not "60s".
        assert_eq!(format_compact(60), "1m");
        assert_eq!(format_compact(59 * 60), "59m");
        // Past an hour the readout stays in minutes rather than rounding to
        // "1h" -- the deciding case for the format (issue #171).
        assert_eq!(format_compact(90 * 60), "90m");
        assert_eq!(format_compact(99 * 60 + 59), "99m");
        // Minutes stop where the second digit does.
        assert_eq!(format_compact(100 * 60), "1h");
        assert_eq!(format_compact(119 * 60), "1h");
        assert_eq!(format_compact(120 * 60), "2h");
        assert_eq!(format_compact(9 * 3600), "9h");
    }

    #[test]
    fn compact_format_rounds_down_so_the_readout_never_overpromises() {
        // A child who sees "2m" must not find the session ending in 61
        // seconds' time; truncation is the safe direction.
        assert_eq!(format_compact(119), "1m");
        assert_eq!(format_compact(2 * 3600 - 1), "1h");
    }

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(0), "00:00");
        assert_eq!(format_duration(59), "00:59");
        assert_eq!(format_duration(60), "01:00");
        assert_eq!(format_duration(3599), "59:59");
        assert_eq!(format_duration(3600), "01:00:00");
        assert_eq!(format_duration(3661), "01:01:01");
    }
}
