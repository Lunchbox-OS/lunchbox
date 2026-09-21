//! Time badges: the pills that say what a category or an activity costs, has
//! banked, or still needs.
//!
//! The rule the branding sets is that **time lives where it applies** — on the
//! compartment when the gate is the category's, on the item when the gate is
//! the item's own — and never in a panel of its own. This module is only the
//! drawing of one pill; deciding which pill a thing wears is `Badge::for_*`.

use gtk4::glib;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use lunchbox_api::{ReasonCode, TokenStatus};
use std::cell::Cell;
use std::f64::consts::PI;
use std::time::Duration;

use crate::theme;

/// What a pill is saying. The colour follows from this and nothing else:
/// yellow is "you can", deep teal is "banked", putty is "not yet".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Badge {
    /// Time spent here banks time toward something else.
    Earn,
    /// This much is banked and ready to spend.
    Bank { minutes: u64 },
    /// Have against need, both in whole minutes.
    Need { have: u64, need: u64 },
    /// Cooling down; this long to wait.
    Wait { minutes: u64 },
}

impl Badge {
    /// The badge a token gate deserves right now, if any.
    ///
    /// Below the threshold it is a have/need pill, because what the child needs
    /// to know is how much further there is to go. At or above it, the need is
    /// met and the same number becomes a bank pill — the brief's "cap the need
    /// pill at the threshold: once earned, it becomes a bank pill".
    pub fn for_tokens(status: &TokenStatus) -> Option<Self> {
        let have = whole_minutes(status.balance);
        let need = whole_minutes(status.minimum);

        if status.unlocked {
            return Some(Badge::Bank { minutes: have });
        }
        // A gate with no minimum is "any balance at all opens it", so there is
        // no ratio to show — it is simply out of banked time.
        if need == 0 {
            return Some(Badge::Bank { minutes: have });
        }
        Some(Badge::Need {
            have: have.min(need),
            need,
        })
    }

    /// A cooldown pill, when that is what is holding an activity shut. Checked
    /// before the token gate by callers: a cooldown is the nearer obstacle, and
    /// a banked balance the child cannot spend for another eight minutes is a
    /// misleading thing to show them.
    pub fn for_cooldown(
        reasons: &[ReasonCode],
        now: chrono::DateTime<chrono::Local>,
    ) -> Option<Self> {
        reasons.iter().find_map(|r| match unwrap_group(r) {
            ReasonCode::CooldownActive { available_at } => {
                let remaining = *available_at - now;
                // Round *up*: a pill reading "0m" on an activity that is still
                // shut is the one number that cannot be true.
                let minutes = (remaining.num_seconds().max(0) as f64 / 60.0).ceil() as u64;
                Some(Badge::Wait {
                    minutes: minutes.max(1),
                })
            }
            _ => None,
        })
    }

    /// The CSS modifier that colours this pill.
    fn css_class(&self) -> &'static str {
        match self {
            Badge::Earn => "lb-badge--earn",
            Badge::Bank { .. } => "lb-badge--bank",
            Badge::Need { .. } => "lb-badge--need",
            Badge::Wait { .. } => "lb-badge--wait",
        }
    }

    /// The text beside the coin. Bank and wait pills carry the `m` suffix;
    /// have/need is a bare ratio, which reads as minutes in context.
    fn text(&self) -> String {
        match self {
            Badge::Earn => "+".to_string(),
            Badge::Bank { minutes } | Badge::Wait { minutes } => format!("{minutes}m"),
            Badge::Need { have, need } => format!("{have}/{need}"),
        }
    }

    /// Whether the coin should be drawn in cream rather than yellow — it sits
    /// on deep teal on a bank pill, where yellow-on-teal is the only pairing in
    /// the palette that loses its ink ring.
    fn coin_on_dark(&self) -> bool {
        matches!(self, Badge::Bank { .. })
    }

    /// Build the widget for this badge.
    pub fn widget(&self, scale: f64) -> gtk4::Widget {
        let pill = gtk4::Box::new(gtk4::Orientation::Horizontal, theme::px(5, scale));
        pill.add_css_class("lb-badge");
        pill.add_css_class(self.css_class());
        pill.set_halign(gtk4::Align::Center);
        pill.set_valign(gtk4::Align::Center);

        let coin = Coin::new(self.coin_on_dark());
        let size = theme::px(16, scale);
        coin.set_size_request(size, size);
        coin.set_valign(gtk4::Align::Center);
        pill.append(&coin);

        let label = gtk4::Label::new(Some(&self.text()));
        label.set_valign(gtk4::Align::Center);
        pill.append(&label);

        pill.upcast()
    }
}

/// Whole minutes, rounded *down*, as every pill in the branding is written.
/// Down rather than nearest because these are promises: 89 seconds banked is
/// one minute the child can actually spend, not two.
fn whole_minutes(d: Duration) -> u64 {
    d.as_secs() / 60
}

/// See past `GroupRestricted` to the restriction it wraps. A category's
/// cooldown and an activity's own look identical to the child, and they should:
/// the difference is whose limit it is, which is a caregiver's question.
fn unwrap_group(reason: &ReasonCode) -> &ReasonCode {
    match reason {
        ReasonCode::GroupRestricted { reason, .. } => unwrap_group(reason),
        other => other,
    }
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct Coin {
        /// Drawn cream instead of yellow, for a coin on the deep-teal pill.
        pub on_dark: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Coin {
        const NAME: &'static str = "LunchboxCoin";
        type Type = super::Coin;
        type ParentType = gtk4::Widget;
    }

    impl ObjectImpl for Coin {}

    impl WidgetImpl for Coin {
        /// A disc with an ink ring and two clock hands. Cairo rather than an
        /// SVG asset because it has to be legible at 16 px on a 3 px-outlined
        /// pill, where hinting an asset down would close the hands up.
        fn snapshot(&self, snapshot: &gtk4::Snapshot) {
            let obj = self.obj();
            let w = obj.width() as f64;
            let h = obj.height() as f64;
            if w <= 0.0 || h <= 0.0 {
                return;
            }

            let bounds = gtk4::graphene::Rect::new(0.0, 0.0, w as f32, h as f32);
            let cr = snapshot.append_cairo(&bounds);

            let d = w.min(h);
            let cx = w / 2.0;
            let cy = h / 2.0;
            // The ring is drawn *on* the circumference, so the disc is inset by
            // half its width to keep the whole coin inside the widget.
            let ring = (d * 0.12).max(1.0);
            let r = d / 2.0 - ring / 2.0;

            let (fr, fg, fb) = if self.on_dark.get() {
                theme::CREAM_RGB
            } else {
                theme::YELLOW_RGB
            };
            let (ir, ig, ib) = theme::INK_RGB;

            cr.arc(cx, cy, r, 0.0, 2.0 * PI);
            cr.set_source_rgb(fr, fg, fb);
            let _ = cr.fill_preserve();
            cr.set_source_rgb(ir, ig, ib);
            cr.set_line_width(ring);
            let _ = cr.stroke();

            // Hands at roughly ten-past-two: asymmetric, so the glyph reads as
            // a clock rather than a plus sign at small sizes.
            cr.set_line_width((d * 0.10).max(1.0));
            cr.set_line_cap(gtk4::cairo::LineCap::Round);
            cr.move_to(cx, cy);
            cr.line_to(cx, cy - r * 0.50);
            let _ = cr.stroke();
            cr.move_to(cx, cy);
            cr.line_to(cx + r * 0.38, cy + r * 0.20);
            let _ = cr.stroke();
        }
    }
}

glib::wrapper! {
    /// The little clock face that leads every pill.
    pub struct Coin(ObjectSubclass<imp::Coin>)
        @extends gtk4::Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget;
}

impl Coin {
    pub fn new(on_dark: bool) -> Self {
        let obj: Self = glib::Object::builder().build();
        obj.imp().on_dark.set(on_dark);
        obj
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(balance_s: u64, minimum_s: u64, unlocked: bool) -> TokenStatus {
        TokenStatus {
            balance: Duration::from_secs(balance_s),
            minimum: Duration::from_secs(minimum_s),
            unlocked,
            max_balance: None,
            carry_over: false,
        }
    }

    #[test]
    fn an_unmet_gate_shows_how_far_there_is_to_go() {
        let b = Badge::for_tokens(&status(5 * 60, 10 * 60, false)).unwrap();
        assert_eq!(b, Badge::Need { have: 5, need: 10 });
        assert_eq!(b.text(), "5/10");
    }

    #[test]
    fn a_met_gate_becomes_a_bank_pill() {
        let b = Badge::for_tokens(&status(25 * 60, 10 * 60, true)).unwrap();
        assert_eq!(b, Badge::Bank { minutes: 25 });
        assert_eq!(b.text(), "25m");
    }

    #[test]
    fn minutes_round_down_so_a_pill_never_over_promises() {
        // 89s is one minute the child can actually spend, not two.
        let b = Badge::for_tokens(&status(89, 0, true)).unwrap();
        assert_eq!(b, Badge::Bank { minutes: 1 });
    }

    #[test]
    fn a_gate_with_no_minimum_has_no_ratio_to_show() {
        // "Any balance above zero opens it" — so there is no target to count
        // toward, only what is left.
        let b = Badge::for_tokens(&status(0, 0, false)).unwrap();
        assert_eq!(b, Badge::Bank { minutes: 0 });
    }

    #[test]
    fn the_need_pill_never_exceeds_its_threshold() {
        // The engine can report a balance above the minimum while the gate is
        // still shut (a relock mid-evaluation, issue #193). "12/10" would be
        // nonsense; the pill caps.
        let b = Badge::for_tokens(&status(12 * 60, 10 * 60, false)).unwrap();
        assert_eq!(b, Badge::Need { have: 10, need: 10 });
    }

    #[test]
    fn a_cooldown_rounds_up_and_never_reads_zero() {
        let now = lunchbox_util::now();
        let reasons = vec![ReasonCode::CooldownActive {
            available_at: now + chrono::Duration::seconds(61),
        }];
        assert_eq!(
            Badge::for_cooldown(&reasons, now),
            Some(Badge::Wait { minutes: 2 })
        );

        // Seconds from the end it is still shut, so it may not say "0m".
        let nearly = vec![ReasonCode::CooldownActive {
            available_at: now + chrono::Duration::seconds(3),
        }];
        assert_eq!(
            Badge::for_cooldown(&nearly, now),
            Some(Badge::Wait { minutes: 1 })
        );
    }

    #[test]
    fn a_cooldown_inherited_from_the_category_counts_as_one() {
        let now = lunchbox_util::now();
        let reasons = vec![ReasonCode::GroupRestricted {
            group: lunchbox_util::GroupId::new("attention-heavy"),
            label: "Games".into(),
            reason: Box::new(ReasonCode::CooldownActive {
                available_at: now + chrono::Duration::seconds(8 * 60),
            }),
        }];
        assert_eq!(
            Badge::for_cooldown(&reasons, now),
            Some(Badge::Wait { minutes: 8 })
        );
    }

    #[test]
    fn colour_follows_meaning() {
        assert_eq!(Badge::Earn.css_class(), "lb-badge--earn");
        assert_eq!(Badge::Bank { minutes: 1 }.css_class(), "lb-badge--bank");
        assert_eq!(
            Badge::Need { have: 1, need: 2 }.css_class(),
            "lb-badge--need"
        );
        // Only the deep-teal pill needs the cream coin.
        assert!(Badge::Bank { minutes: 1 }.coin_on_dark());
        assert!(!Badge::Earn.coin_on_dark());
    }
}
