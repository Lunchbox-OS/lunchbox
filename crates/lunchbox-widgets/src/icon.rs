//! An activity's icon, with an ink keyline traced around its silhouette.
//!
//! Two surfaces draw one: the launcher, at 64px in a compartment (issue #207),
//! and the HUD, at 34px beside the name of the running activity (issue #209).
//! Both want the same three things — work out what to draw for an entry, dilate
//! its alpha into a keyline, and draw the icon on top — so it lives here rather
//! than in one of them.
//!
//! Size is a number and colour is CSS, as everything in this crate is: the
//! keyline is drawn in whatever `Widget::color()` resolves to, which is ink on
//! the launcher's cream compartment and cream on the HUD's ink bar. A keyline's
//! job is to separate the icon from what is behind it, and what is behind it is
//! not the same in both places.

use gtk4::glib;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use lunchbox_api::EntryView;
use std::cell::{Cell, RefCell};

/// How long the press animation runs before the launch actually goes out.
const PRESS_MS: u64 = 120;

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

            // The keyline's colour is the widget's own, so a stylesheet
            // decides it: ink on the launcher's cream compartment, cream on the
            // HUD's ink bar. `color` inherits in GTK CSS, so neither has to say
            // anything unless it wants something other than the type colour
            // around it.
            let rgba = obj.color();
            let outline = gtk4::graphene::Vec4::new(rgba.red(), rgba.green(), rgba.blue(), 0.0);
            // All zeros but the alpha passthrough: every pixel becomes the
            // outline colour at its own alpha, i.e. a solid silhouette.
            let mut m = [0.0f32; 16];
            m[15] = 1.0;
            let silhouette = gtk4::graphene::Matrix::from_float(m);

            // The offset shadow under a pressed icon (5 x 6 px).
            if lift > 0.0 {
                snapshot.save();
                snapshot.translate(&gtk4::graphene::Point::new(
                    x + 5.0 * ui * lift,
                    y + 6.0 * ui * lift,
                ));
                snapshot.push_color_matrix(&silhouette, &outline);
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
                    snapshot.push_color_matrix(&silhouette, &outline);
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
    /// The icon slot: paints the icon and the keyline around it.
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
    ///
    /// The launcher's, not the HUD's — a bar does not press anything — but it
    /// lives with the widget it animates.
    pub fn lift(&self) {
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

/// Work out what to draw for an entry: a file on disk, a theme icon, or the
/// fallback for its kind.
///
/// `px` is the size the theme is asked to look an icon up at. It is a hint, not
/// a promise — a theme may only have one size of a given icon, and a file on
/// disk has whatever size it has — so the caller still decides what to draw it
/// at.
pub fn resolve_icon(entry: &EntryView, px: i32) -> Option<gtk4::gdk::Paintable> {
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
            return Some(lookup(&theme, icon_ref, px).upcast());
        }
    }

    Some(lookup(&gtk4::IconTheme::for_display(&display), fallback, px).upcast())
}

fn lookup(theme: &gtk4::IconTheme, name: &str, px: i32) -> gtk4::IconPaintable {
    theme.lookup_icon(
        name,
        &[],
        px,
        1,
        gtk4::TextDirection::None,
        gtk4::IconLookupFlags::empty(),
    )
}
