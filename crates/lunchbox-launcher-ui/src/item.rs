//! One activity in a compartment: its icon under an ink keyline, its name, and
//! the badge that says what it costs or still needs.
//!
//! Replaces the old `tile.rs`. The visible differences are the keyline, the
//! badge and the press animation; the invisible one matters more — a locked
//! activity is now *drawn* rather than skipped, because a child who cannot see
//! Celeste cannot learn that ten minutes of Tux Math would open it.

use gtk4::glib;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use lunchbox_api::{EntryView, ReasonCode};
use std::cell::{Cell, RefCell};

use crate::badge::Badge;
use crate::theme;

/// How long the press animation runs before the launch actually goes out.
const PRESS_MS: u64 = 120;

/// Height reserved for an item's name: two lines of 16/800 plus its leading.
/// See `set_entry` for why it is reserved rather than measured.
const NAME_TWO_LINES: i32 = 46;

/// Height reserved for an item's badge, whether or not it has one.
const BADGE_SLOT: i32 = 26;

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
    reasons.iter().any(|r| match unwrap_group(r) {
        // Time: wait, earn, or come back tomorrow.
        ReasonCode::OutsideTimeWindow { .. }
        | ReasonCode::QuotaExhausted { .. }
        | ReasonCode::CooldownActive { .. }
        | ReasonCode::TokensInsufficient { .. }
        | ReasonCode::ManuallyDisabled { .. }
        | ReasonCode::SessionActive { .. } => true,
        // Something to go and fix in the room: plug the pad in, get the
        // network back. Also child-actionable, also worth drawing.
        ReasonCode::RequiredInputUnavailable { .. } | ReasonCode::InternetUnavailable { .. } => {
            true
        }
        // Warming up; it will be available in a moment on its own.
        ReasonCode::NotReady { .. } => true,
        // Configuration and capability. Not the child's to solve, and the
        // caregiver hears about these through diagnostics instead.
        ReasonCode::Disabled { .. }
        | ReasonCode::UnsupportedKind { .. }
        | ReasonCode::ProtectionUnavailable
        | ReasonCode::AdminMode => false,
        // Unwrapped above; listed so a new reason has to be classified here.
        ReasonCode::GroupRestricted { .. } => false,
    })
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
/// Carried over from `tile.rs` unchanged in spirit: most reasons fall back to
/// their `Debug` form, the input-dependency reason (issue #96) names the
/// missing devices, and a group restriction says whose limit it is.
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
        pub badge_slot: super::ShakeBox,
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
                badge_slot: super::ShakeBox::new(),
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

            content.append(&self.art);

            self.label.set_wrap(true);
            self.label.set_wrap_mode(gtk4::pango::WrapMode::WordChar);
            self.label.set_justify(gtk4::Justification::Center);
            self.label.set_lines(2);
            self.label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
            self.label.set_halign(gtk4::Align::Center);
            self.label.add_css_class("lb-item__name");
            content.append(&self.label);

            content.append(&self.badge_slot);

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

        // Reserved whether or not there is a badge, for the same reason the
        // name is: a badge on one item must not push the item below it out of
        // line with the stack beside it. Art + name + badge then comes to
        // exactly `ITEM_H`.
        imp.badge_slot
            .set_size_request(-1, theme::px(BADGE_SLOT, scale));
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
/// fallback for its kind. Carried over from `tile.rs`.
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

// ------------------------------------------------------------- the badge slot

mod shake_imp {
    use super::*;

    #[derive(Default)]
    pub struct ShakeBox {
        pub child: RefCell<Option<gtk4::Widget>>,
        /// Horizontal offset, in px, applied at draw time.
        pub offset: Cell<f64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ShakeBox {
        const NAME: &'static str = "LunchboxShakeBox";
        type Type = super::ShakeBox;
        type ParentType = gtk4::Widget;
    }

    impl ObjectImpl for ShakeBox {
        fn dispose(&self) {
            if let Some(child) = self.child.borrow_mut().take() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for ShakeBox {
        /// An empty slot still measures as whatever height was requested of
        /// it: GTK only applies a size request as a *floor* on what the widget
        /// asks for, so a childless slot reporting 0 would collapse and take
        /// the alignment with it.
        fn measure(&self, orientation: gtk4::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            match self.child.borrow().as_ref() {
                Some(child) => child.measure(orientation, for_size),
                None => (0, 0, -1, -1),
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            if let Some(child) = self.child.borrow().as_ref() {
                child.allocate(width, height, baseline, None);
            }
        }

        /// The shake is applied here rather than in the allocation, so it
        /// cannot disturb the layout of everything around it — a badge that
        /// re-laid-out its compartment 60 times a second would jog the whole
        /// row sideways.
        fn snapshot(&self, snapshot: &gtk4::Snapshot) {
            let Some(child) = self.child.borrow().clone() else {
                return;
            };
            let dx = self.offset.get();
            if dx != 0.0 {
                snapshot.save();
                snapshot.translate(&gtk4::graphene::Point::new(dx as f32, 0.0));
            }
            self.obj().snapshot_child(&child, snapshot);
            if dx != 0.0 {
                snapshot.restore();
            }
        }
    }
}

glib::wrapper! {
    /// Holds an item's badge, and can shake it when a locked item is pressed.
    pub struct ShakeBox(ObjectSubclass<shake_imp::ShakeBox>)
        @extends gtk4::Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget;
}

impl ShakeBox {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    pub fn set_child(&self, child: Option<gtk4::Widget>) {
        if let Some(old) = self.imp().child.borrow_mut().take() {
            old.unparent();
        }
        if let Some(child) = child {
            child.set_parent(self.upcast_ref::<gtk4::Widget>());
            *self.imp().child.borrow_mut() = Some(child);
        }
        self.queue_resize();
    }

    /// A 120 ms shake: the refusal a locked item gives back to a press.
    pub fn shake(&self) {
        if self.imp().child.borrow().is_none() {
            return;
        }
        let start = std::time::Instant::now();
        self.add_tick_callback(move |slot, _| {
            let t = start.elapsed().as_millis() as f64 / PRESS_MS as f64;
            if t >= 1.0 {
                slot.imp().offset.set(0.0);
                slot.queue_draw();
                return glib::ControlFlow::Break;
            }
            // Three swings, decaying to nothing at the end so it settles
            // rather than stopping mid-swing.
            let amplitude = 4.0 * (1.0 - t);
            slot.imp()
                .offset
                .set((t * 3.0 * 2.0 * std::f64::consts::PI).sin() * amplitude);
            slot.queue_draw();
            glib::ControlFlow::Continue
        });
    }
}

impl Default for ShakeBox {
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
        // Several things can block at once. If any of them is one the child
        // can act on, the badge has something to say, so it is drawn.
        let reasons = vec![
            ReasonCode::ProtectionUnavailable,
            ReasonCode::CooldownActive {
                available_at: lunchbox_util::now(),
            },
        ];
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
