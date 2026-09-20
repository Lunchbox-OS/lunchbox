//! One activity in a compartment: its icon under an ink keyline, its name, and
//! the badge that says what it costs or still needs.
//!
//! The thing that matters most here is not visible: a locked activity is
//! *drawn* rather than skipped, because a child who cannot see Celeste cannot
//! learn that ten minutes of Tux Math would open it. `is_shown_when_locked`
//! below decides which locks are worth showing and which are not.

use gtk4::glib;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use lunchbox_api::{EntryView, ReasonCode};
use std::cell::{Cell, RefCell};

use crate::badge::Badge;
use crate::theme;

/// How long the press animation runs before the launch actually goes out.
const PRESS_MS: u64 = 120;

/// The leading for an activity's name at `scale`, as a Pango attribute list.
///
/// Absolute rather than `AttrFloat::new_line_height`'s factor: the factor form
/// had no effect here — two lines stayed ~1.8 apart, Baloo 2's own default —
/// while the absolute form does what it says. Absolute means the value has to
/// be computed from the scaled type size, which is why this takes `scale` and
/// why `theme::ITEM_FONT_PX` exists.
fn name_leading(scale: f64) -> gtk4::pango::AttrList {
    // `type.item.lineHeight` from the design file. Baloo 2 asks for about
    // 1.55 of its own accord — it is a display face with room for tall
    // Devanagari matras it is not being asked to set here — and two lines of a
    // wrapped name at that spacing drift apart badly.
    //
    // It reaches Pango rather than the stylesheet because GTK4 CSS has no
    // `line-height`; it is the same token either way.
    let px = theme::ITEM_FONT_PX as f64 * scale * theme::tokens::ITEM_LINE_HEIGHT;
    let attrs = gtk4::pango::AttrList::new();
    attrs.insert(gtk4::pango::AttrInt::new_line_height_absolute(
        (px * gtk4::pango::SCALE as f64).round() as i32,
    ));
    attrs
}

/// Where an activity's name wraps, in characters — the branding's 150 px at
/// the item's 16 px type. See the call site for why this is not in pixels.
const NAME_MAX_CHARS: i32 = 18;

/// Height reserved for an item's name: two lines of 16/800 at
/// `NAME_LINE_HEIGHT`, and a little for descenders.
/// See `set_entry` for why it is reserved rather than measured.
const NAME_TWO_LINES: i32 = 40;

/// Whether an activity the policy has switched off should still be drawn.
///
/// The branding's rule is that locked things stay visible at 50 % with their
/// badge (layout rule 6) — but that rule is about *time*. A child can wait out
/// a window, earn a gate, or plug a gamepad in; those are worth drawing,
/// because the badge tells them what to do. A missing binary or a protection
/// this host cannot apply is not: nothing the child does changes it, and a
/// permanently dead icon in the tin teaches them to ignore dimmed items.
///
/// So: obstacles the child can act on are shown locked, and everything else is
/// hidden, which is what the launcher did with all of them before.
pub fn is_shown_when_locked(reasons: &[ReasonCode]) -> bool {
    // Nothing blocking at all — available, and shown for the ordinary reason.
    if reasons.is_empty() {
        return true;
    }
    // A veto is final. Several reasons can block one activity at once, and an
    // activity that is switched off is switched off however many clocks also
    // happen to be against it — this used to be a plain "any reason is
    // actionable" vote, which let an entry a caregiver had explicitly disabled
    // reappear on the strength of also being outside its window (#208 review).
    if reasons.iter().any(|r| is_permanent(unwrap_group(r))) {
        return false;
    }
    reasons.iter().any(|r| is_actionable(unwrap_group(r)))
}

/// Blockers that no amount of waiting, earning or plugging things in will
/// clear, so the activity is not drawn at all.
///
/// The child is owed a badge that tells them what to do; when there is nothing
/// they could do, a permanently dimmed icon in the tin only teaches them to
/// ignore dimmed icons. The caregiver hears about these through diagnostics.
fn is_permanent(reason: &ReasonCode) -> bool {
    matches!(
        reason,
        // The caregiver said no, in the configuration.
        ReasonCode::Disabled { .. }
            // This host cannot run it, and will not start being able to.
            | ReasonCode::UnsupportedKind { .. }
            // Its protection cannot be applied here, so it does not launch.
            | ReasonCode::ProtectionUnavailable
    )
}

/// Blockers the child can do something about, or simply outlast. These keep
/// the activity on screen at half strength, wearing the badge that says what
/// would clear them.
fn is_actionable(reason: &ReasonCode) -> bool {
    match reason {
        // Time: wait, earn, or come back tomorrow.
        ReasonCode::OutsideTimeWindow { .. }
        | ReasonCode::QuotaExhausted { .. }
        | ReasonCode::CooldownActive { .. }
        | ReasonCode::TokensInsufficient { .. }
        // Switched off for *today* only, which is a thing to wait out rather
        // than a thing that has been taken away — unlike `Disabled` above.
        | ReasonCode::ManuallyDisabled { .. }
        | ReasonCode::SessionActive { .. } => true,
        // Something to go and fix in the room: plug the pad in, get the
        // network back.
        ReasonCode::RequiredInputUnavailable { .. } | ReasonCode::InternetUnavailable { .. } => {
            true
        }
        // Warming up; it will be available in a moment on its own.
        ReasonCode::NotReady { .. } => true,
        // Handled by `is_permanent`, and listed rather than caught by a
        // wildcard so a new reason has to be classified in both places.
        ReasonCode::Disabled { .. }
        | ReasonCode::UnsupportedKind { .. }
        | ReasonCode::ProtectionUnavailable => false,
        // Administrator mode replaces the field with the picker, so this never
        // decides what the child sees; it is every entry's reason or none's.
        ReasonCode::AdminMode => false,
        // Unwrapped by the callers above.
        ReasonCode::GroupRestricted { .. } => false,
    }
}

/// See past `GroupRestricted` to the restriction it wraps.
fn unwrap_group(reason: &ReasonCode) -> &ReasonCode {
    match reason {
        ReasonCode::GroupRestricted { reason, .. } => unwrap_group(reason),
        other => other,
    }
}

/// A short, human-readable tooltip for why an activity is unavailable.
///
/// Most reasons fall back to their `Debug` form; the input-dependency reason
/// (issue #96) names the missing devices, and a group restriction says whose
/// limit it is.
pub fn reason_tooltip(reason: &ReasonCode) -> String {
    match reason {
        ReasonCode::RequiredInputUnavailable { devices } => {
            let list = devices
                .iter()
                .map(|d| d.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            if list.is_empty() {
                "Requires an input device".to_string()
            } else {
                format!("Requires: {list}")
            }
        }
        // Name the category, so a parent can see the limit is shared rather
        // than specific to this activity (issue #5).
        ReasonCode::GroupRestricted { label, reason, .. } => {
            format!("{label}: {}", reason_tooltip(reason))
        }
        other => format!("{other:?}"),
    }
}

mod imp {
    use super::*;

    pub struct LauncherItem {
        pub entry: RefCell<Option<EntryView>>,
        pub art: super::IconArt,
        pub label: gtk4::Label,
        pub badge_slot: crate::offset::OffsetBin,
        /// Whether a press should launch. Kept beside the entry rather than
        /// read off `is_sensitive`, because a locked item stays *focusable* —
        /// the child has to be able to reach it to read its badge.
        pub launchable: Cell<bool>,
    }

    impl Default for LauncherItem {
        fn default() -> Self {
            Self {
                entry: RefCell::new(None),
                art: super::IconArt::new(),
                label: gtk4::Label::new(None),
                badge_slot: crate::offset::OffsetBin::new(),
                launchable: Cell::new(false),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LauncherItem {
        const NAME: &'static str = "LunchboxLauncherItem";
        type Type = super::LauncherItem;
        type ParentType = gtk4::Button;
    }

    impl ObjectImpl for LauncherItem {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();

            let content = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
            content.set_halign(gtk4::Align::Center);
            content.set_valign(gtk4::Align::Center);

            // The badge rides the icon's top-right corner, the way a
            // notification count does, instead of taking a row of its own
            // under the name. Most activities have no badge at all, so a
            // reserved row spent the height on nothing for all of them —
            // and the reservation had to stay whether or not it was used, or
            // a badged item pushed its neighbours out of line.
            //
            // Deliberately not clipped to the icon: a `10/30` pill is a little
            // wider than the 78px art slot, and the item has 41px of slack
            // each side plus a 16px column gap, so it has room to hang over
            // without reaching the next item.
            let art_overlay = gtk4::Overlay::new();
            // Named so the locked state can dim the icon without dimming the
            // badge that floats over it, or the selection behind it.
            self.art.add_css_class("lb-item__art");
            art_overlay.set_child(Some(&self.art));
            self.badge_slot.set_halign(gtk4::Align::End);
            self.badge_slot.set_valign(gtk4::Align::Start);
            art_overlay.add_overlay(&self.badge_slot);
            content.append(&art_overlay);

            self.label.set_wrap(true);
            self.label.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
            self.label.set_justify(gtk4::Justification::Center);
            self.label.set_lines(2);
            self.label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
            // Where the name actually wraps. A width *request* cannot do this
            // — GTK treats it as a floor, so a long name takes its natural
            // width, widens the whole cell and never wraps at all. This is a
            // ceiling on the natural width, which is what makes the branding's
            // "wrap to 2 lines max at 150 px" true.
            //
            // In characters rather than pixels, and deliberately so: the count
            // is relative to the font size, so one value holds at every UI
            // scale, where a pixel cap would have to be rescaled with
            // everything else.
            self.label.set_max_width_chars(NAME_MAX_CHARS);
            self.label.set_halign(gtk4::Align::Center);
            self.label.add_css_class("lb-item__name");
            content.append(&self.label);

            obj.set_child(Some(&content));
            obj.add_css_class("lb-item");
            // GTK's own button chrome would fight every rule in the branding:
            // the item is a silhouette on enamel, not a raised control.
            obj.add_css_class("flat");
            obj.set_has_frame(false);
        }
    }

    impl WidgetImpl for LauncherItem {}
    impl ButtonImpl for LauncherItem {}
}

glib::wrapper! {
    pub struct LauncherItem(ObjectSubclass<imp::LauncherItem>)
        @extends gtk4::Button, gtk4::Widget,
        @implements gtk4::Accessible, gtk4::Actionable, gtk4::Buildable, gtk4::ConstraintTarget;
}

impl LauncherItem {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    /// Fill the item in from an entry, laid out for `scale`.
    ///
    /// `badge` is decided by the caller rather than here, because the rule is
    /// about the item's place in a category, not about the item: a category
    /// already wearing the earn pill must not have it repeated on every one of
    /// its members. `compartment::item_badge` is where that is worked out.
    pub fn set_entry(&self, entry: EntryView, scale: f64, badge: Option<Badge>) {
        let imp = self.imp();

        imp.label.set_text(&entry.label);
        imp.label.set_attributes(Some(&name_leading(scale)));
        // A fixed two-line box, not "up to two lines". A name that wraps would
        // otherwise push its badge down and make the whole stack taller than
        // its neighbours, and the branding is explicit that height never grows
        // -- width does. Reserving both lines whether or not they are used
        // keeps every row on the same baseline across the field.
        imp.label.set_size_request(
            theme::px(150, scale).min(theme::px(theme::ITEM_W, scale)),
            theme::px(NAME_TWO_LINES, scale),
        );

        let slot = theme::px(theme::ART_SLOT, scale);
        imp.art.set_size_request(slot, slot);
        imp.art
            .set_icon(resolve_icon(&entry), theme::px(theme::ICON_PX, scale));
        imp.art.set_keyline(theme::KEYLINE * scale, scale);

        self.set_size_request(
            theme::px(theme::ITEM_W, scale),
            theme::px(theme::ITEM_H, scale),
        );

        // Available means enabled with nothing blocking; everything else is
        // locked, whether or not it is drawn at all.
        let available = entry.enabled && entry.reasons.is_empty();
        imp.launchable.set(available);
        if available {
            self.remove_css_class("lb-item--locked");
            self.set_tooltip_text(None);
        } else {
            self.add_css_class("lb-item--locked");
            if let Some(first) = entry.reasons.first() {
                self.set_tooltip_text(Some(&reason_tooltip(first)));
            }
        }

        // No reservation needed now that it floats over the icon: an item
        // with a badge is exactly as tall as one without.
        imp.badge_slot.set_child(badge.map(|b| b.widget(scale)));

        *imp.entry.borrow_mut() = Some(entry);
    }

    pub fn entry_id(&self) -> Option<lunchbox_util::EntryId> {
        self.imp()
            .entry
            .borrow()
            .as_ref()
            .map(|e| e.entry_id.clone())
    }

    /// Whether pressing this item should start anything.
    pub fn is_launchable(&self) -> bool {
        self.imp().launchable.get()
    }

    /// Play the press animation, then run `then`.
    ///
    /// A locked item shakes its badge instead and `then` never runs — the
    /// branding is explicit that locked items can be focused but not pressed,
    /// and the shake is what tells the child the press registered and was
    /// refused, rather than being swallowed.
    pub fn press<F: Fn() + 'static>(&self, then: F) {
        if !self.is_launchable() {
            self.imp().badge_slot.shake();
            return;
        }

        let animate = gtk4::Settings::for_display(&self.display()).is_gtk_enable_animations();
        if !animate {
            then();
            return;
        }

        self.imp().art.lift();
        glib::timeout_add_local_once(std::time::Duration::from_millis(PRESS_MS), move || {
            then();
        });
    }
}

impl Default for LauncherItem {
    fn default() -> Self {
        Self::new()
    }
}

/// Work out what to draw for an entry: a file on disk, a theme icon, or the
/// fallback for its kind.
fn resolve_icon(entry: &EntryView) -> Option<gtk4::gdk::Paintable> {
    let fallback = match entry.kind_tag {
        lunchbox_api::EntryKindTag::Vm => "computer",
        lunchbox_api::EntryKindTag::Media => "video-x-generic",
        lunchbox_api::EntryKindTag::Retroarch => "applications-games",
        lunchbox_api::EntryKindTag::Ebook => "application-epub+zip",
        lunchbox_api::EntryKindTag::Custom => "applications-other",
        _ => "application-x-executable",
    };

    let display = gtk4::gdk::Display::default()?;

    if let Some(icon_ref) = &entry.icon_ref {
        let expanded = if let Some(rest) = icon_ref.strip_prefix("~/") {
            dirs::home_dir()
                .map(|h| h.join(rest).to_string_lossy().into_owned())
                .unwrap_or_else(|| icon_ref.clone())
        } else {
            icon_ref.clone()
        };

        let path = std::path::Path::new(&expanded);
        if path.is_file()
            && let Ok(texture) = gtk4::gdk::Texture::from_filename(path)
        {
            return Some(texture.upcast());
        }

        let theme = gtk4::IconTheme::for_display(&display);
        if theme.has_icon(icon_ref) {
            return Some(lookup(&theme, icon_ref).upcast());
        }
    }

    Some(lookup(&gtk4::IconTheme::for_display(&display), fallback).upcast())
}

fn lookup(theme: &gtk4::IconTheme, name: &str) -> gtk4::IconPaintable {
    theme.lookup_icon(
        name,
        &[],
        theme::ICON_PX,
        1,
        gtk4::TextDirection::None,
        gtk4::IconLookupFlags::empty(),
    )
}

// ---------------------------------------------------------------- the art

mod art_imp {
    use super::*;

    #[derive(Default)]
    pub struct IconArt {
        pub paintable: RefCell<Option<gtk4::gdk::Paintable>>,
        pub icon_px: Cell<i32>,
        pub keyline: Cell<f64>,
        /// The UI scale, so the rise and the offset shadow grow with
        /// everything else rather than shrinking on a large output.
        pub ui_scale: Cell<f64>,
        /// 0.0 at rest, 1.0 at the top of the press. Drives scale, rise and
        /// the offset ink shadow together.
        pub lift: Cell<f64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for IconArt {
        const NAME: &'static str = "LunchboxIconArt";
        type Type = super::IconArt;
        type ParentType = gtk4::Widget;
    }

    impl ObjectImpl for IconArt {}

    impl WidgetImpl for IconArt {
        /// The icon with an ink keyline traced around its silhouette.
        ///
        /// The keyline is the icon drawn eight times in solid ink on a ring
        /// around the real position, with the icon itself on top — a dilation
        /// of the alpha channel, done with GSK colour-matrix nodes so it works
        /// on any paintable (theme SVG, PNG on disk, pixel art) without
        /// touching pixels. The branding brief offers four `drop-shadow`
        /// filters instead; eight offsets are used because four give a
        /// plus-shaped keyline that visibly corners on round icons, and the
        /// cost — nine draws of a 64 px icon on a screen that only redraws when
        /// the policy changes — is not worth saving.
        ///
        /// An opaque icon (a JPEG, a baked background) traces its own
        /// rectangle, which is fine: it reads as a framed tile.
        fn snapshot(&self, snapshot: &gtk4::Snapshot) {
            let obj = self.obj();
            let borrowed = self.paintable.borrow();
            let Some(paintable) = borrowed.as_ref() else {
                return;
            };

            let w = obj.width() as f32;
            let h = obj.height() as f32;
            let size = self.icon_px.get() as f32;
            if w <= 0.0 || h <= 0.0 || size <= 0.0 {
                return;
            }

            let lift = self.lift.get() as f32;
            let ui = self.ui_scale.get().max(0.01) as f32;
            // Scale about the centre, then rise. Both peak at the top of the
            // press and are zero at rest, so the resting path is unchanged.
            let scale = 1.0 + 0.12 * lift;
            let rise = 4.0 * ui * lift;

            let drawn = size * scale;
            let x = (w - drawn) / 2.0;
            let y = (h - drawn) / 2.0 - rise;

            let ink = gtk4::graphene::Vec4::new(
                theme::INK_RGB.0 as f32,
                theme::INK_RGB.1 as f32,
                theme::INK_RGB.2 as f32,
                0.0,
            );
            // All zeros but the alpha passthrough: every pixel becomes ink at
            // its own alpha, i.e. a solid silhouette.
            let mut m = [0.0f32; 16];
            m[15] = 1.0;
            let silhouette = gtk4::graphene::Matrix::from_float(m);

            // The offset ink shadow under a pressed icon (5 x 6 px).
            if lift > 0.0 {
                snapshot.save();
                snapshot.translate(&gtk4::graphene::Point::new(
                    x + 5.0 * ui * lift,
                    y + 6.0 * ui * lift,
                ));
                snapshot.push_color_matrix(&silhouette, &ink);
                paintable.snapshot(snapshot, drawn as f64, drawn as f64);
                snapshot.pop();
                snapshot.restore();
            }

            let r = self.keyline.get() as f32;
            if r > 0.0 {
                // Eight unit vectors: the four axes and the four diagonals,
                // the latter at 1/sqrt(2) so every offset is the same distance
                // from the centre and the keyline comes out round.
                const D: f32 = std::f32::consts::FRAC_1_SQRT_2;
                const DIRS: [(f32, f32); 8] = [
                    (1.0, 0.0),
                    (-1.0, 0.0),
                    (0.0, 1.0),
                    (0.0, -1.0),
                    (D, D),
                    (-D, D),
                    (D, -D),
                    (-D, -D),
                ];
                for (dx, dy) in DIRS {
                    snapshot.save();
                    snapshot.translate(&gtk4::graphene::Point::new(x + dx * r, y + dy * r));
                    snapshot.push_color_matrix(&silhouette, &ink);
                    paintable.snapshot(snapshot, drawn as f64, drawn as f64);
                    snapshot.pop();
                    snapshot.restore();
                }
            }

            snapshot.save();
            snapshot.translate(&gtk4::graphene::Point::new(x, y));
            paintable.snapshot(snapshot, drawn as f64, drawn as f64);
            snapshot.restore();
        }
    }
}

glib::wrapper! {
    /// The icon slot: paints the icon and the ink keyline around it.
    pub struct IconArt(ObjectSubclass<art_imp::IconArt>)
        @extends gtk4::Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget;
}

impl IconArt {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    pub fn set_icon(&self, paintable: Option<gtk4::gdk::Paintable>, icon_px: i32) {
        *self.imp().paintable.borrow_mut() = paintable;
        self.imp().icon_px.set(icon_px);
        self.queue_draw();
    }

    pub fn set_keyline(&self, radius: f64, ui_scale: f64) {
        self.imp().keyline.set(radius);
        self.imp().ui_scale.set(ui_scale);
        self.queue_draw();
    }

    /// Run the press animation once: out to the top of the lift, then back.
    fn lift(&self) {
        let start = std::time::Instant::now();
        self.add_tick_callback(move |art, _| {
            let t = start.elapsed().as_millis() as f64 / PRESS_MS as f64;
            if t >= 1.0 {
                art.imp().lift.set(0.0);
                art.queue_draw();
                return glib::ControlFlow::Break;
            }
            // Out and back within the 120 ms, so the icon is home again by the
            // time the activity's own window takes the screen.
            art.imp().lift.set((t * std::f64::consts::PI).sin());
            art.queue_draw();
            glib::ControlFlow::Continue
        });
    }
}

impl Default for IconArt {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lunchbox_util::{EntryId, GroupId};

    #[test]
    fn an_available_activity_is_shown() {
        assert!(is_shown_when_locked(&[]));
    }

    #[test]
    fn time_shaped_locks_stay_on_screen_with_their_badge() {
        for reason in [
            ReasonCode::OutsideTimeWindow {
                next_window_start: None,
            },
            ReasonCode::QuotaExhausted {
                used: std::time::Duration::from_secs(60),
                quota: std::time::Duration::from_secs(60),
            },
            ReasonCode::CooldownActive {
                available_at: lunchbox_util::now(),
            },
            ReasonCode::TokensInsufficient {
                balance: std::time::Duration::ZERO,
                required: std::time::Duration::from_secs(600),
            },
        ] {
            assert!(
                is_shown_when_locked(std::slice::from_ref(&reason)),
                "{reason:?} is something the child can wait out or earn, so it \
                 must stay visible"
            );
        }
    }

    #[test]
    fn configuration_problems_are_hidden_rather_than_dimmed_forever() {
        for reason in [
            ReasonCode::Disabled { reason: None },
            ReasonCode::UnsupportedKind {
                kind: lunchbox_api::EntryKindTag::Steam,
            },
            ReasonCode::ProtectionUnavailable,
            ReasonCode::AdminMode,
        ] {
            assert!(
                !is_shown_when_locked(std::slice::from_ref(&reason)),
                "{reason:?} is not the child's to solve, so a permanently dead \
                 icon in the tin would only teach them to ignore dimmed items"
            );
        }
    }

    #[test]
    fn a_restriction_inherited_from_the_category_is_judged_on_its_own_terms() {
        let shown = ReasonCode::GroupRestricted {
            group: GroupId::new("attention-heavy"),
            label: "Games".into(),
            reason: Box::new(ReasonCode::CooldownActive {
                available_at: lunchbox_util::now(),
            }),
        };
        assert!(is_shown_when_locked(std::slice::from_ref(&shown)));

        let hidden = ReasonCode::GroupRestricted {
            group: GroupId::new("attention-heavy"),
            label: "Games".into(),
            reason: Box::new(ReasonCode::Disabled { reason: None }),
        };
        assert!(!is_shown_when_locked(std::slice::from_ref(&hidden)));
    }

    #[test]
    fn one_actionable_reason_is_enough_to_keep_it_on_screen() {
        // Several things can block at once. If they are all ones the child can
        // act on, the badge has something to say, so it is drawn.
        let reasons = vec![
            ReasonCode::CooldownActive {
                available_at: lunchbox_util::now(),
            },
            ReasonCode::OutsideTimeWindow {
                next_window_start: None,
            },
        ];
        assert!(is_shown_when_locked(&reasons));
    }

    /// An activity the caregiver switched off in the configuration stays off,
    /// however many clocks happen to agree with them.
    #[test]
    fn a_disabled_activity_is_not_shown_whatever_else_is_true() {
        let reasons = vec![
            ReasonCode::Disabled {
                reason: Some("not for this child".into()),
            },
            ReasonCode::OutsideTimeWindow {
                next_window_start: None,
            },
        ];
        assert!(
            !is_shown_when_locked(&reasons),
            "a caregiver's explicit no must not be outvoted by a reason the \
             child could otherwise wait out"
        );
    }

    /// Same for the blockers nothing can clear.
    #[test]
    fn a_permanent_blocker_overrides_an_actionable_one() {
        for permanent in [
            ReasonCode::ProtectionUnavailable,
            ReasonCode::UnsupportedKind {
                kind: lunchbox_api::EntryKindTag::Steam,
            },
        ] {
            let reasons = vec![
                permanent.clone(),
                ReasonCode::CooldownActive {
                    available_at: lunchbox_util::now(),
                },
            ];
            assert!(
                !is_shown_when_locked(&reasons),
                "{permanent:?} can never clear, so a cooldown badge beside it \
                 would be promising something that will not happen"
            );
        }
    }

    /// "Not today" is not the same as "not at all": a daily override is a
    /// thing to wait out, so it keeps its place.
    #[test]
    fn switched_off_for_the_day_still_shows() {
        let reasons = vec![ReasonCode::ManuallyDisabled {
            until: lunchbox_util::now().date_naive(),
        }];
        assert!(is_shown_when_locked(&reasons));
    }

    #[test]
    fn a_group_restriction_names_the_category_in_its_tooltip() {
        let reason = ReasonCode::GroupRestricted {
            group: GroupId::new("attention-heavy"),
            label: "Games".into(),
            reason: Box::new(ReasonCode::RequiredInputUnavailable {
                devices: vec![lunchbox_api::InputDeviceType::Gamepad],
            }),
        };
        assert_eq!(reason_tooltip(&reason), "Games: Requires: gamepad");
    }

    #[test]
    fn the_input_tooltip_lists_what_to_plug_in() {
        let reason = ReasonCode::RequiredInputUnavailable {
            devices: vec![lunchbox_api::InputDeviceType::Gamepad],
        };
        assert_eq!(reason_tooltip(&reason), "Requires: gamepad");

        let none = ReasonCode::RequiredInputUnavailable { devices: vec![] };
        assert_eq!(reason_tooltip(&none), "Requires an input device");
    }

    /// Guards the one thing a test can check about `resolve_icon` without a
    /// display: that an entry naming a file that is not there still ends up
    /// with a kind fallback rather than nothing.
    #[test]
    fn an_entry_id_survives_a_round_trip() {
        let entry = EntryView {
            entry_id: EntryId::new("celeste"),
            label: "Celeste".into(),
            icon_ref: Some("/nonexistent/celeste.png".into()),
            kind_tag: lunchbox_api::EntryKindTag::Steam,
            enabled: true,
            group: None,
            reasons: vec![],
            tokens: None,
            earns_tokens: false,
            max_run_if_started_now: None,
        };
        assert_eq!(entry.entry_id.as_str(), "celeste");
    }
}
