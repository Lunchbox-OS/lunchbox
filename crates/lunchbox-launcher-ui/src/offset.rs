//! A single-child container that draws its child shifted sideways.
//!
//! Two things in the launcher need to move something horizontally without
//! moving anything else: an item's badge, which shakes when a locked activity
//! is pressed, and a compartment's header, which slides along to stay on screen
//! while the compartment it belongs to scrolls past (#208 review).
//!
//! Both do it in `snapshot` rather than in the allocation, and for the same
//! reason: a widget that re-laid-out its parent sixty times a second would jog
//! the whole row sideways. Drawing at an offset costs nothing and disturbs
//! nobody.

use gtk4::glib;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use std::cell::{Cell, RefCell};

/// How long a refusal shake lasts.
const SHAKE_MS: u64 = 120;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct OffsetBin {
        pub child: RefCell<Option<gtk4::Widget>>,
        /// Horizontal offset, in px, applied at draw time.
        pub offset: Cell<f64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for OffsetBin {
        const NAME: &'static str = "LunchboxOffsetBin";
        type Type = super::OffsetBin;
        type ParentType = gtk4::Widget;
    }

    impl ObjectImpl for OffsetBin {
        fn dispose(&self) {
            if let Some(child) = self.child.borrow_mut().take() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for OffsetBin {
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
    pub struct OffsetBin(ObjectSubclass<imp::OffsetBin>)
        @extends gtk4::Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget;
}

impl OffsetBin {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    /// Wrap a widget in one of these.
    pub fn around(child: &impl IsA<gtk4::Widget>) -> Self {
        let bin = Self::new();
        bin.set_child(Some(child.clone().upcast()));
        bin
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

    pub fn has_child(&self) -> bool {
        self.imp().child.borrow().is_some()
    }

    /// Draw the child this far to the right of where it was allocated.
    /// Cheap enough to call on every scroll frame; a no-op when unchanged.
    pub fn set_offset(&self, dx: f64) {
        if (self.imp().offset.get() - dx).abs() < 0.5 {
            return;
        }
        self.imp().offset.set(dx);
        self.queue_draw();
    }

    /// A 120 ms shake: the refusal a locked item gives back to a press.
    pub fn shake(&self) {
        if !self.has_child() {
            return;
        }
        let start = std::time::Instant::now();
        self.add_tick_callback(move |bin, _| {
            let t = start.elapsed().as_millis() as f64 / SHAKE_MS as f64;
            if t >= 1.0 {
                bin.imp().offset.set(0.0);
                bin.queue_draw();
                return glib::ControlFlow::Break;
            }
            // Three swings, decaying to nothing at the end so it settles
            // rather than stopping mid-swing.
            let amplitude = 4.0 * (1.0 - t);
            bin.imp()
                .offset
                .set((t * 3.0 * 2.0 * std::f64::consts::PI).sin() * amplitude);
            bin.queue_draw();
            glib::ControlFlow::Continue
        });
    }
}

impl Default for OffsetBin {
    fn default() -> Self {
        Self::new()
    }
}
