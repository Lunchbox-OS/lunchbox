//! A label turned a quarter turn counter-clockwise, for the vertical HUD.
//!
//! # Why this widget exists
//!
//! GTK3 could rotate a label with `gtk_label_set_angle`. **GTK4 removed it**,
//! and there is no replacement property — so the sideways activity title that
//! issue #171 asks for has to be built.
//!
//! Three routes were available:
//!
//! 1. `GtkFixed::set_child_transform`, which does rotate a child — but
//!    `GtkFixed` measures its children *unrotated*, so the container would
//!    need its size request faked on both axes and would never negotiate
//!    correctly with the box around it.
//! 2. A `GtkDrawingArea` rendering the text through Pango by hand. That works,
//!    but forfeits both things the activity title depends on: the CSS-driven
//!    font size that follows the HUD scale factor, and `EllipsizeMode::End`,
//!    which is what keeps a long book title from pushing the end-session
//!    button off the bar (see the comments on `app_label` in `app.rs`).
//! 3. This: a `GtkWidget` subclass wrapping a real `GtkLabel`, swapping the
//!    axes in `measure` and handing the child a rotation in `size_allocate`.
//!
//! Route 3 keeps the child a genuine `GtkLabel`, so it styles, ellipsizes and
//! scales exactly like the horizontal one — the rotation is purely a matter of
//! how it is measured and placed.
//!
//! # The geometry
//!
//! The child is allocated with our axes swapped (its width is our height) and
//! then transformed by `translate(0, height) · rotate(-90°)`. In GSK a
//! positive angle turns clockwise on screen, so `-90°` is the quarter turn to
//! the left that the issue asks for, and the translate puts the child's origin
//! back at the bottom-left corner. A child point `(x, y)` therefore lands at
//! `(y, height - x)`:
//!
//! ```text
//!   child (0,0)      -> (0, height)   bottom-left, where the text starts
//!   child (W_c,0)    -> (0, 0)        top-left, where the text ends
//! ```
//!
//! so the text reads bottom-to-top up the left edge of the screen, which is
//! what a bar rotated to the left produces.

use gtk4::glib;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;

mod imp {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    pub struct RotatedLabel {
        pub label: RefCell<Option<gtk4::Label>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RotatedLabel {
        const NAME: &'static str = "LunchboxRotatedLabel";
        type Type = super::RotatedLabel;
        type ParentType = gtk4::Widget;
    }

    impl ObjectImpl for RotatedLabel {
        fn constructed(&self) {
            self.parent_constructed();

            let label = gtk4::Label::new(None);
            label.set_parent(&*self.obj());
            *self.label.borrow_mut() = Some(label);
        }

        fn dispose(&self) {
            // A `GtkWidget` subclass owns its children explicitly; without
            // this GTK warns that the widget is finalized with a child still
            // attached.
            if let Some(label) = self.label.borrow_mut().take() {
                label.unparent();
            }
        }
    }

    impl WidgetImpl for RotatedLabel {
        /// Measure the child on the *other* axis. Our height is the label's
        /// natural width (how long the text is), and our width is its height
        /// (how tall a line of it is) — which is exactly what rotating it a
        /// quarter turn does.
        fn measure(&self, orientation: gtk4::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let Some(label) = self.label.borrow().clone() else {
                return (0, 0, -1, -1);
            };
            let swapped = match orientation {
                gtk4::Orientation::Horizontal => gtk4::Orientation::Vertical,
                _ => gtk4::Orientation::Horizontal,
            };
            let (min, nat, _, _) = label.measure(swapped, for_size);
            // Baselines are meaningless once the text is turned on its side:
            // there is no horizontal line for a neighbour to align to.
            (min, nat, -1, -1)
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            let Some(label) = self.label.borrow().clone() else {
                return;
            };
            // See the module docs for the derivation. The child is allocated
            // in its own (unrotated) coordinate space with our axes swapped,
            // and the transform maps that space onto our allocation.
            let transform = gtk4::gsk::Transform::new()
                .translate(&gtk4::graphene::Point::new(0.0, height as f32))
                .rotate(-90.0);
            label.allocate(height, width, -1, Some(transform));
        }

        /// Report the child's request on the axis we did *not* get asked
        /// about. GTK asks height-for-width or width-for-height depending on
        /// this, and getting it wrong makes an ellipsizing label collapse.
        fn request_mode(&self) -> gtk4::SizeRequestMode {
            gtk4::SizeRequestMode::WidthForHeight
        }
    }
}

glib::wrapper! {
    pub struct RotatedLabel(ObjectSubclass<imp::RotatedLabel>)
        @extends gtk4::Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget;
}

impl RotatedLabel {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    /// The wrapped `GtkLabel`, for the styling and ellipsize setup the
    /// horizontal title does too.
    pub fn label(&self) -> gtk4::Label {
        self.imp()
            .label
            .borrow()
            .clone()
            .expect("label is built in constructed()")
    }

    pub fn set_text(&self, text: &str) {
        self.label().set_text(text);
    }
}

impl Default for RotatedLabel {
    fn default() -> Self {
        Self::new()
    }
}
