//! Time display widget
//!
//! Shows elapsed time, remaining time, or countdown.

use crate::theme::Urgency;
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
        /// How loudly to show it, or `None` for the ordinary cream. Set from
        /// the session's warning state (issue #209) — see `set_urgency`.
        pub urgency: Cell<Option<Urgency>>,
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

    /// Whether there is anything to count down to.
    ///
    /// The bar asks because the countdown shares the middle with two other
    /// things: a warning, which takes its place, and the wall clock, which
    /// moves into the middle when neither of them is there.
    pub fn has_remaining(&self) -> bool {
        self.imp().remaining_secs.borrow().is_some()
    }

    /// Show the countdown as a warning, or not.
    ///
    /// This used to be decided here, from the number of seconds left: yellow
    /// under five minutes, putty and blinking under one. Those thresholds are
    /// the same numbers the example config warns at, and the two systems
    /// crossed them on the same tick in opposite directions — a yellow "1
    /// minute remaining!" toast over a countdown that had just gone putty. So
    /// the countdown no longer has an opinion about time: it is told, from the
    /// session's own warning state, through the one mapping in `theme::Urgency`
    /// that the toast goes through too.
    ///
    /// The consequence worth knowing: **an activity with no warnings configured
    /// has a cream countdown all the way down**. The bar says what the policy
    /// says, and a policy that says nothing is a device that was told not to
    /// interrupt. `config.example.toml` ships `[[service.default_warnings]]`,
    /// so a device built from it is unaffected.
    pub fn set_urgency(&self, urgency: Option<Urgency>) {
        let imp = self.imp();
        if imp.urgency.get() == urgency {
            return;
        }
        imp.urgency.set(urgency);
        self.update_display();
    }

    /// Update the display based on current state
    fn update_display(&self) {
        let imp = self.imp();

        if let Some(label) = imp.label.borrow().as_ref() {
            let remaining = *imp.remaining_secs.borrow();

            let compact = imp.compact.get();
            // Nothing to count down to — no session, or an activity with no
            // time limit — shows nothing at all. The bar used to say `--:--`,
            // which is a countdown's way of saying it has no news; the idle bar
            // in §8 of the branding brief simply has no countdown on it. The
            // label is hidden rather than blanked so the box around it
            // collapses instead of leaving a gap on the bar.
            match remaining {
                Some(secs) => {
                    label.set_text(&if compact {
                        format_compact(secs)
                    } else {
                        format_remaining(secs)
                    });
                    label.set_visible(true);
                }
                None => label.set_visible(false),
            }

            for urgency in Urgency::ALL {
                label.remove_css_class(urgency.countdown_class());
            }
            if let Some(urgency) = imp.urgency.get() {
                label.add_css_class(urgency.countdown_class());
            }
        }
    }
}

impl Default for TimeDisplay {
    fn default() -> Self {
        Self::new()
    }
}

/// How much is left, in words: `12:40 left`.
///
/// From §8 of the branding brief, and a change of kind rather than of spelling.
/// The bar used to carry a bare `MM:SS` in a monospace face, which is a
/// stopwatch — a thing that counts, with no opinion about what the number is
/// for. A child reading this one wants to know how long they have, so the
/// readout says so, in the bar's own face.
///
/// The leading unit loses its zero for the same reason: `5:03 left` is how the
/// time would be said out loud, and `05:03` is how an instrument would print it.
/// Everything after the leading unit keeps both digits, because that is what
/// makes it a time rather than two numbers.
fn format_remaining(secs: u64) -> String {
    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    let seconds = secs % 60;

    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02} left")
    } else {
        format!("{minutes}:{seconds:02} left")
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
        // a fourth character does not fit across the bar. Walk a full day.
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
    fn the_countdown_says_what_it_is_counting() {
        // The brief's own example.
        assert_eq!(format_remaining(12 * 60 + 40), "12:40 left");
        assert_eq!(format_remaining(0), "0:00 left");
        assert_eq!(format_remaining(59), "0:59 left");
        assert_eq!(format_remaining(60), "1:00 left");
        assert_eq!(format_remaining(3599), "59:59 left");
        // Past an hour the hours become the leading unit, and the minutes take
        // the second digit the seconds always had.
        assert_eq!(format_remaining(3600), "1:00:00 left");
        assert_eq!(format_remaining(3661), "1:01:01 left");
        assert_eq!(format_remaining(10 * 3600), "10:00:00 left");
    }
}
