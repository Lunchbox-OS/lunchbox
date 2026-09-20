//! The flat grid of items used by administrator mode's application picker.
//!
//! This is *not* the child's home screen — that is `field.rs`, a row of
//! compartments. The picker is a searchable list of every `.desktop` file on
//! the system, which is a different problem: there is no category to put a
//! browser plugin's helper in, there are fifty of them, and the person looking
//! at it is a caregiver setting the device up rather than a child choosing
//! something to do. A wrapping flow box is the right shape for that, so it
//! keeps one, wearing the branding's item widget and palette.

use gtk4::glib;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use lunchbox_api::EntryView;
use lunchbox_util::EntryId;
use std::cell::RefCell;
use std::rc::Rc;

use crate::item::LauncherItem;

mod imp {
    use super::*;

    type LaunchCallback = Rc<RefCell<Option<Box<dyn Fn(EntryId) + 'static>>>>;

    pub struct LauncherGrid {
        pub flow_box: gtk4::FlowBox,
        pub items: RefCell<Vec<LauncherItem>>,
        pub on_launch: LaunchCallback,
    }

    impl Default for LauncherGrid {
        fn default() -> Self {
            Self {
                flow_box: gtk4::FlowBox::new(),
                items: RefCell::new(Vec::new()),
                on_launch: Rc::new(RefCell::new(None)),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LauncherGrid {
        const NAME: &'static str = "LunchboxLauncherGrid";
        type Type = super::LauncherGrid;
        type ParentType = gtk4::Box;
    }

    impl ObjectImpl for LauncherGrid {
        fn constructed(&self) {
            self.parent_constructed();

            let obj = self.obj();
            obj.set_orientation(gtk4::Orientation::Vertical);
            obj.set_hexpand(true);
            obj.set_vexpand(true);

            self.flow_box.set_homogeneous(true);
            self.flow_box
                .set_selection_mode(gtk4::SelectionMode::Single);
            self.flow_box.set_max_children_per_line(7);
            self.flow_box.set_min_children_per_line(2);
            self.flow_box.set_row_spacing(16);
            self.flow_box.set_column_spacing(16);
            self.flow_box.set_halign(gtk4::Align::Center);
            self.flow_box.set_valign(gtk4::Align::Start);
            self.flow_box.set_hexpand(true);

            // Follow the flow box's own selection, however it changes —
            // keyboard, click, or `select_first` after a search.
            let obj_weak = obj.downgrade();
            self.flow_box.connect_selected_children_changed(move |_| {
                if let Some(obj) = obj_weak.upgrade() {
                    obj.mark_selection();
                }
            });

            // Unlike the child's field, this one may scroll vertically: there
            // is no bound on how many applications are installed.
            let scrolled = gtk4::ScrolledWindow::new();
            scrolled.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
            scrolled.set_child(Some(&self.flow_box));
            scrolled.set_hexpand(true);
            scrolled.set_vexpand(true);

            obj.append(&scrolled);
        }
    }

    impl WidgetImpl for LauncherGrid {}
    impl BoxImpl for LauncherGrid {}
}

glib::wrapper! {
    pub struct LauncherGrid(ObjectSubclass<imp::LauncherGrid>)
        @extends gtk4::Box, gtk4::Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget, gtk4::Orientable;
}

impl LauncherGrid {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    /// Set the callback for when an application is chosen.
    pub fn connect_launch<F: Fn(EntryId) + 'static>(&self, callback: F) {
        *self.imp().on_launch.borrow_mut() = Some(Box::new(callback));
    }

    /// Replace the contents of the picker.
    pub fn set_entries(&self, entries: Vec<EntryView>, scale: f64) {
        let imp = self.imp();

        while let Some(child) = imp.flow_box.first_child() {
            imp.flow_box.remove(&child);
        }
        imp.items.borrow_mut().clear();

        for entry in entries {
            let item = LauncherItem::new();
            // No badge: these are `.desktop` files, not activities, so there is
            // no gate, no schedule and nothing banked against them.
            item.set_entry(entry, scale, None);

            let on_launch = imp.on_launch.clone();
            item.connect_clicked(move |item| {
                let Some(entry_id) = item.entry_id() else {
                    return;
                };
                let on_launch = on_launch.clone();
                item.press(move || {
                    if let Some(callback) = on_launch.borrow().as_ref() {
                        callback(entry_id.clone());
                    }
                });
            });

            imp.flow_box.insert(&item, -1);
            imp.items.borrow_mut().push(item);
        }

        self.select_first();
    }

    pub fn select_first(&self) {
        let imp = self.imp();
        if let Some(child) = imp.flow_box.child_at_index(0) {
            imp.flow_box.select_child(&child);
        }
        self.mark_selection();
    }

    /// Put the branding's selected look on whichever item the flow box has
    /// selected.
    ///
    /// The field manages this class itself as the cursor moves; the picker has
    /// no cursor of its own, so it follows `GtkFlowBox`'s selection instead.
    /// Without it the picker falls back to the *theme's* selection colour,
    /// which is whatever the distribution picked — orange, on Ubuntu — against
    /// a cream-and-enamel palette.
    fn mark_selection(&self) {
        let imp = self.imp();
        let selected: Vec<i32> = imp
            .flow_box
            .selected_children()
            .iter()
            .map(|c| c.index())
            .collect();
        for (i, item) in imp.items.borrow().iter().enumerate() {
            if selected.contains(&(i as i32)) {
                item.add_css_class(crate::field::SELECTED_CLASS);
            } else {
                item.remove_css_class(crate::field::SELECTED_CLASS);
            }
        }
    }
}

impl Default for LauncherGrid {
    fn default() -> Self {
        Self::new()
    }
}
