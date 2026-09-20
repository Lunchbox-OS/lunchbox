//! The field: the row of compartments the child actually looks at.
//!
//! Also what administrator mode's application picker is made of, handed a
//! single synthetic category holding everything installed — so the sunk wells,
//! the selected cell, the scrolling and its fades are the same code in both
//! places rather than two things kept in step.
//!
//! Two rules drive the whole of this file:
//!
//! 1. **Never scroll vertically.** Categories are columns; a category with more
//!    than three members grows *wider*, and if the row overflows, the row
//!    scrolls sideways.
//! 2. **At most one item is selected, and not until something has selected
//!    it.** The launcher comes up with no selection at all and wakes on the
//!    first direction press, hover or tap (#208 review); a tap on empty space
//!    puts it away again. A locked item can be selected but not pressed —
//!    being able to reach it is how the child reads the badge that says what
//!    would unlock it.
//!
//! The branding asks for exactly one item focused at all times, which is right
//! for a D-pad and wrong for a touchscreen: there is no cursor there to explain
//! a standing highlight, and it claims a choice nobody has made.

use gtk4::glib;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use lunchbox_api::{EntryView, GroupView};
use lunchbox_util::EntryId;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::compartment;
use crate::item::{LauncherItem, is_shown_when_locked};
use crate::theme;

/// The name of the implicit category that collects everything belonging to no
/// group. Last in the row, always: a caregiver who has not sorted their
/// activities into categories still gets a tin rather than an error.
const UNGROUPED_LABEL: &str = "Everything else";

/// How long a chevron chip takes to slide out of its edge.
const CHIP_SLIDE_MS: u32 = 160;

/// How long the row takes to ease from one scroll position to the next.
const SCROLL_MS: u64 = 220;

/// A compartment's own border and padding: how close its header may sit to
/// either of its ends.
const COMPARTMENT_EDGE: i32 = 20;

/// How far the selection is kept from either edge when scrolling to it.
/// Matches the width of `.lb-field__fade`, so the selected item is never the
/// thing the fade is dissolving.
const EDGE_MARGIN: i32 = 56;

/// The class that draws the selected cell. See theme.rs for why the launcher
/// tracks this itself instead of leaning on `:focus`.
const SELECTED_CLASS: &str = "lb-item--selected";

/// One navigable column of items, and the compartment it sits in.
pub struct Stack {
    /// The well this column belongs to, held so scrolling can bring the whole
    /// category into view rather than just the item — which is what the brief
    /// asks for, and what stops a wide category creeping past the left margin
    /// one stack at a time.
    compartment: gtk4::Widget,
    items: Vec<LauncherItem>,
}

/// A compartment and the header that has to stay on screen with it.
pub struct Header {
    well: gtk4::Widget,
    name: crate::offset::OffsetBin,
    badge: Option<crate::offset::OffsetBin>,
}

/// Decide the categories and their members, in the order they are drawn.
///
/// Config order throughout: groups in policy order, members in policy order,
/// then the ungrouped remainder. A category with nothing to show is dropped
/// entirely rather than drawn as an empty well — an empty compartment says
/// "there is something here" when there isn't.
///
/// Returned as `(label, group, entries)` so the caller can build widgets; split
/// out from the widget building so the ordering is testable without a display.
fn categorise<'a>(
    entries: &[EntryView],
    groups: &'a [GroupView],
) -> Vec<(String, Option<&'a GroupView>, Vec<EntryView>)> {
    let shown: Vec<&EntryView> = entries
        .iter()
        .filter(|e| is_shown_when_locked(&e.reasons))
        .collect();

    let mut out = Vec::new();

    for group in groups {
        let members: Vec<EntryView> = shown
            .iter()
            .filter(|e| e.group.as_ref() == Some(&group.group_id))
            .map(|e| (*e).clone())
            .collect();
        if !members.is_empty() {
            out.push((group.label.clone(), Some(group), members));
        }
    }

    // Anything whose group the daemon did not report lands here too, not just
    // entries with no group at all: a member of a vanished category is still an
    // activity, and dropping it would be the one failure the child cannot work
    // around.
    let known: Vec<_> = groups.iter().map(|g| &g.group_id).collect();
    let leftovers: Vec<EntryView> = shown
        .iter()
        .filter(|e| match &e.group {
            None => true,
            Some(id) => !known.contains(&id),
        })
        .map(|e| (*e).clone())
        .collect();
    if !leftovers.is_empty() {
        out.push((UNGROUPED_LABEL.to_string(), None, leftovers));
    }

    out
}

/// How far a chevron press moves the row.
///
/// Most of a screenful, less one item kept as overlap so there is something in
/// common between the view you left and the one you arrive at. An earlier cut
/// moved a single item width, which on a touchscreen reads as the press not
/// having worked: a chevron is a "next page" control, not a "next thing" one.
///
/// Falls back to half a screen on the degenerate viewport where one item is
/// wider than the page.
fn scroll_step(page: f64, scale: f64) -> f64 {
    let overlap = theme::px(theme::ITEM_W, scale) as f64;
    if page <= overlap {
        return (page * 0.5).max(1.0);
    }
    page - overlap
}

/// Slide something rightwards so it starts no further left than `min_x`.
///
/// Used for a category's name, which holds the left edge of whatever part of
/// its compartment is on screen. It never moves left of where it was laid out,
/// and never so far right that it would pass `max_right`.
///
/// All coordinates are the row's, which is also what the scroll adjustment
/// speaks, so no conversion is needed anywhere.
fn leading_offset(min_x: f64, natural_x: f64, width: f64, max_right: f64) -> f64 {
    let wanted = min_x.max(natural_x);
    let capped = wanted.min((max_right - width).max(natural_x));
    capped - natural_x
}

/// Slide something leftwards so it ends no further right than `max_x`.
///
/// The mirror of `leading_offset`, for a category's badge: it sits at the far
/// end of the header, so it holds the *right* edge of what is visible. Without
/// this it would be off the screen for the whole time the compartment's tail
/// was, which is exactly when its balance is worth reading.
fn trailing_offset(max_x: f64, natural_x: f64, width: f64, min_left: f64) -> f64 {
    let wanted = (max_x - width).min(natural_x);
    let capped = wanted.max(min_left.min(natural_x));
    capped - natural_x
}

mod imp {
    use super::*;

    type LaunchCallback = Rc<RefCell<Option<Box<dyn Fn(EntryId) + 'static>>>>;

    pub struct LauncherField {
        pub scroller: gtk4::ScrolledWindow,
        pub row: gtk4::Box,
        pub more_left: gtk4::Button,
        pub more_right: gtk4::Button,
        /// What slides each chip in and out from its edge.
        pub reveal_left: gtk4::Revealer,
        pub reveal_right: gtk4::Revealer,
        /// The soft edge the row disappears under, one per side. Shown with
        /// the chip above it, and on the same condition.
        pub fade_left: gtk4::Box,
        pub fade_right: gtk4::Box,
        pub stacks: RefCell<Vec<Stack>>,
        /// One per compartment, in row order. See `slide_headers`.
        pub headers: RefCell<Vec<Header>>,
        /// (stack, row within it). Meaningless when `stacks` is empty.
        pub cursor: Cell<(usize, usize)>,
        pub on_launch: LaunchCallback,
        /// What the child started last. Focus returns here when the field comes
        /// back, which is what makes "play one more round" a single press.
        pub last_launched: RefCell<Option<EntryId>>,
        pub scale: Cell<f64>,
        /// Set when the field is rebuilt, cleared once the selection has
        /// actually been scrolled into view. See `wire_scroll_chips`.
        pub pending_scroll: Cell<bool>,
        /// Whether the selection is *shown*. The cursor always has a position;
        /// this is whether the child has done anything to deserve seeing it.
        pub selection_active: Cell<bool>,
        /// Bumped whenever a scroll animation starts, so an older one stops.
        pub scroll_generation: Cell<u64>,
        /// The snapshot the field is currently drawing, kept so a change of
        /// scale can redraw it without waiting for the daemon to say anything
        /// new. The launcher is fullscreen on an output whose size it learns
        /// only once it is mapped, so the first layout after startup is always
        /// a re-layout.
        pub last_state: RefCell<(Vec<EntryView>, Vec<GroupView>)>,
    }

    impl Default for LauncherField {
        fn default() -> Self {
            Self {
                scroller: gtk4::ScrolledWindow::new(),
                row: gtk4::Box::new(gtk4::Orientation::Horizontal, 0),
                more_left: gtk4::Button::new(),
                more_right: gtk4::Button::new(),
                reveal_left: gtk4::Revealer::new(),
                reveal_right: gtk4::Revealer::new(),
                fade_left: gtk4::Box::new(gtk4::Orientation::Vertical, 0),
                fade_right: gtk4::Box::new(gtk4::Orientation::Vertical, 0),
                stacks: RefCell::new(Vec::new()),
                headers: RefCell::new(Vec::new()),
                cursor: Cell::new((0, 0)),
                on_launch: Rc::new(RefCell::new(None)),
                last_launched: RefCell::new(None),
                scale: Cell::new(1.0),
                pending_scroll: Cell::new(false),
                selection_active: Cell::new(false),
                scroll_generation: Cell::new(0),
                last_state: RefCell::new((Vec::new(), Vec::new())),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LauncherField {
        const NAME: &'static str = "LunchboxLauncherField";
        type Type = super::LauncherField;
        type ParentType = gtk4::Widget;
    }

    impl ObjectImpl for LauncherField {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_layout_manager(Some(gtk4::BinLayout::new()));
            obj.add_css_class("lb-field");
            // Claim the space wherever it is put. In the child's shell the
            // field is a stack page and gets the window either way; in
            // administrator mode's picker it shares a box with the search
            // entry, and without this it takes only its natural height — which
            // left a one-result search sitting in a compartment the height of
            // one item rather than a well with room in it.
            obj.set_hexpand(true);
            obj.set_vexpand(true);

            self.row.set_valign(gtk4::Align::Fill);
            self.row.add_css_class("lb-field__row");

            // Horizontal only, and with no visible scrollbar: the row is driven
            // by the D-pad, and a scrollbar under a compartment would be one
            // more thing sitting in the tin.
            self.scroller
                .set_policy(gtk4::PolicyType::External, gtk4::PolicyType::Never);
            self.scroller.set_child(Some(&self.row));
            self.scroller.set_hexpand(true);
            self.scroller.set_vexpand(true);

            // The fade goes on before the chips, so the chips sit above it.
            // A compartment clipped by the viewport edge would otherwise end
            // on a hard vertical cut; under the fade it runs out of the tin
            // instead, which is what says "there is more this way" before the
            // chip is even read.
            for (fade, modifier, align) in [
                (&self.fade_left, "lb-field__fade--left", gtk4::Align::Start),
                (&self.fade_right, "lb-field__fade--right", gtk4::Align::End),
            ] {
                fade.add_css_class("lb-field__fade");
                fade.add_css_class(modifier);
                fade.set_halign(align);
                fade.set_valign(gtk4::Align::Fill);
                fade.set_visible(false);
                // Decoration only: it must never eat a click meant for the
                // compartment underneath it.
                fade.set_can_target(false);
            }

            for (button, icon, align) in [
                (&self.more_left, "pan-start-symbolic", gtk4::Align::Start),
                (&self.more_right, "pan-end-symbolic", gtk4::Align::End),
            ] {
                button.set_child(Some(&gtk4::Image::from_icon_name(icon)));
                button.add_css_class("lb-more");
                button.set_halign(align);
                button.set_valign(gtk4::Align::Center);
                // Not a tab stop: it is a signpost for a row that the D-pad
                // already scrolls, not a control to land on.
                button.set_can_focus(false);
            }

            // Each chip rides a revealer, so it slides out of the edge it
            // belongs to rather than appearing there (#208 review): on a
            // touchscreen a control that blinks into existence reads as a
            // glitch, where one that slides in reads as an invitation.
            //
            // A hidden revealer measures zero, so the chip cannot take a tap
            // while it is away — no `can_target` juggling needed.
            for (revealer, button, transition, align) in [
                (
                    &self.reveal_left,
                    &self.more_left,
                    gtk4::RevealerTransitionType::SlideRight,
                    gtk4::Align::Start,
                ),
                (
                    &self.reveal_right,
                    &self.more_right,
                    gtk4::RevealerTransitionType::SlideLeft,
                    gtk4::Align::End,
                ),
            ] {
                revealer.set_child(Some(button));
                revealer.set_transition_type(transition);
                revealer.set_transition_duration(CHIP_SLIDE_MS);
                revealer.set_reveal_child(false);
                revealer.set_halign(align);
                revealer.set_valign(gtk4::Align::Center);
            }

            let overlay = gtk4::Overlay::new();
            overlay.set_child(Some(&self.scroller));
            overlay.add_overlay(&self.fade_left);
            overlay.add_overlay(&self.fade_right);
            overlay.add_overlay(&self.reveal_left);
            overlay.add_overlay(&self.reveal_right);
            overlay.set_parent(obj.upcast_ref::<gtk4::Widget>());
        }

        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for LauncherField {}
}

glib::wrapper! {
    pub struct LauncherField(ObjectSubclass<imp::LauncherField>)
        @extends gtk4::Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget;
}

impl LauncherField {
    pub fn new() -> Self {
        let obj: Self = glib::Object::builder().build();
        obj.wire_scroll_chips();
        obj.wire_background_taps();
        obj
    }

    /// A tap that lands on no activity puts the selection away (#208 review).
    ///
    /// Decided by asking what is under the point rather than by letting the
    /// press bubble: an item is a button and handles its own press, but the
    /// compartment, the enamel and the header are all plain widgets a press
    /// would reach either way, and none of them is a choice.
    fn wire_background_taps(&self) {
        let click = gtk4::GestureClick::new();
        let field = self.downgrade();
        click.connect_pressed(move |_, _, x, y| {
            let Some(field) = field.upgrade() else {
                return;
            };
            let hit = field.pick(x, y, gtk4::PickFlags::DEFAULT);
            let on_item = hit
                .map(|w| {
                    w.is::<LauncherItem>() || w.ancestor(LauncherItem::static_type()).is_some()
                })
                .unwrap_or(false);
            if !on_item {
                field.clear_selection();
            }
        });
        self.add_controller(click);
    }

    /// Called when an item is pressed and the press is allowed to launch.
    pub fn connect_launch<F: Fn(EntryId) + 'static>(&self, callback: F) {
        *self.imp().on_launch.borrow_mut() = Some(Box::new(callback));
    }

    /// Rebuild the field from a fresh snapshot.
    pub fn set_state(&self, entries: Vec<EntryView>, groups: Vec<GroupView>) {
        *self.imp().last_state.borrow_mut() = (entries.clone(), groups.clone());
        self.rebuild(entries, groups);
    }

    /// Redraw what is already on screen at the current scale. Used when the
    /// window learns its real size, and if the output ever changes under it.
    pub fn relayout(&self) {
        let (entries, groups) = self.imp().last_state.borrow().clone();
        self.rebuild(entries, groups);
    }

    fn rebuild(&self, entries: Vec<EntryView>, groups: Vec<GroupView>) {
        let imp = self.imp();
        let scale = theme::scale_for(self.width(), self.height());
        imp.scale.set(scale);

        // Keep where the child was looking, so a snapshot arriving while they
        // are half way along the row does not throw them back to the start.
        let previous = self.focused_entry_id();

        while let Some(child) = imp.row.first_child() {
            imp.row.remove(&child);
        }
        imp.stacks.borrow_mut().clear();
        imp.row.set_spacing(theme::px(28, scale));

        let categories = categorise(&entries, &groups);
        let mut stacks: Vec<Stack> = Vec::new();
        let mut headers: Vec<Header> = Vec::new();
        for (label, group, members) in categories {
            let built = compartment::build(&label, group, members, scale);
            imp.row.append(&built.widget);
            headers.push(Header {
                well: built.widget.clone(),
                name: built.name,
                badge: built.badge,
            });
            for items in built.stacks {
                stacks.push(Stack {
                    compartment: built.widget.clone(),
                    items,
                });
            }
        }
        *imp.headers.borrow_mut() = headers;

        for (s, stack) in stacks.iter().enumerate() {
            for (r, item) in stack.items.iter().enumerate() {
                self.wire_item(item, s, r);
            }
        }
        *imp.stacks.borrow_mut() = stacks;

        self.clear_selection();
        self.restore_focus(previous);
        // `restore_focus` scrolls, but nothing is allocated yet at this point:
        // the viewport still measures zero, so the scroll is a no-op and an
        // initial selection in a compartment beyond the first screenful would
        // be left off-screen with nothing visibly selected at all. Ask again
        // once the adjustment knows its real size.
        imp.pending_scroll.set(true);
        self.update_scroll_chips();
    }

    /// Hook one item up to the pointer and to the launch path.
    fn wire_item(&self, item: &LauncherItem, stack: usize, row: usize) {
        // Hover is focus: the branding forbids hover-only affordances, so the
        // pointer lands on exactly the same state the D-pad produces.
        let motion = gtk4::EventControllerMotion::new();
        let field = self.downgrade();
        motion.connect_enter(move |_, _, _| {
            if let Some(field) = field.upgrade() {
                field.imp().selection_active.set(true);
                field.imp().cursor.set((stack, row));
                field.focus_cursor();
            }
        });
        item.add_controller(motion);

        let field = self.downgrade();
        item.connect_clicked(move |item| {
            let Some(field) = field.upgrade() else {
                return;
            };
            field.imp().selection_active.set(true);
            field.imp().cursor.set((stack, row));
            field.press(item);
        });
    }

    /// Press an item: animate, then launch — or refuse, if it is locked.
    fn press(&self, item: &LauncherItem) {
        let Some(entry_id) = item.entry_id() else {
            return;
        };
        let on_launch = self.imp().on_launch.clone();
        let last = self.imp().last_launched.clone();
        item.press(move || {
            *last.borrow_mut() = Some(entry_id.clone());
            if let Some(callback) = on_launch.borrow().as_ref() {
                callback(entry_id.clone());
            }
        });
    }

    /// Press whatever is focused. The keyboard and gamepad paths come here.
    pub fn launch_selected(&self) {
        // Nothing is selected until something has been selected. Pressing A on
        // a launcher showing no highlight must not start whatever the cursor
        // happens to be resting on, unseen — it reveals it instead, and the
        // next press starts it.
        if self.wake_selection() {
            self.focus_cursor();
            return;
        }
        if let Some(item) = self.focused_item() {
            self.press(&item);
        }
    }

    /// Move the focus. `dx` steps between stacks, `dy` within one.
    pub fn move_selection(&self, dx: i32, dy: i32) {
        let imp = self.imp();
        if imp.stacks.borrow().is_empty() {
            return;
        }
        // The press that wakes the selection shows where it already is. Moving
        // as well would slide it one step away from the place the child is
        // about to look for it.
        if self.wake_selection() {
            self.focus_cursor();
            return;
        }
        let stacks = imp.stacks.borrow();
        let (mut s, mut r) = imp.cursor.get();
        s = s.min(stacks.len() - 1);

        if dy != 0 {
            // Wraps: a stack is short, and falling off the bottom of three
            // items with nowhere to go is worse than coming back to the top.
            let len = stacks[s].items.len();
            if len == 0 {
                return;
            }
            r = ((r as i32 + dy).rem_euclid(len as i32)) as usize;
        }

        if dx != 0 {
            let next = s as i32 + dx;
            if next < 0 || next >= stacks.len() as i32 {
                // Deliberately does not wrap. Running off the end of the row
                // should feel like the end of the row; nudging the scroll shows
                // whether there is in fact anything more over there.
                drop(stacks);
                self.nudge(dx);
                return;
            }
            s = next as usize;
            // Keep the row the child was on where the next stack is shorter.
            r = r.min(stacks[s].items.len().saturating_sub(1));
        }

        drop(stacks);
        imp.cursor.set((s, r));
        self.focus_cursor();
    }

    /// Give keyboard focus to the item under the cursor and scroll it in.
    /// Reveal the selection if it is not already showing.
    ///
    /// The launcher boots, and returns from an activity, with no selection at
    /// all (#208 review). A highlight sitting on the first activity before
    /// anyone has touched anything is noise on a touchscreen, where there is no
    /// cursor to explain it — and it suggests a choice has been made when none
    /// has. The cursor still *has* a position throughout; this is only whether
    /// it is drawn.
    ///
    /// Returns whether this call is what woke it, so a first key press can
    /// reveal where the selection is instead of moving it somewhere else.
    fn wake_selection(&self) -> bool {
        if self.imp().selection_active.get() {
            return false;
        }
        self.imp().selection_active.set(true);
        true
    }

    /// Put the selection away again: nothing is selected until the next input.
    pub fn clear_selection(&self) {
        if !self.imp().selection_active.get() {
            return;
        }
        self.imp().selection_active.set(false);
        for stack in self.imp().stacks.borrow().iter() {
            for item in &stack.items {
                item.remove_css_class(SELECTED_CLASS);
            }
        }
    }

    fn focus_cursor(&self) {
        let Some(item) = self.focused_item() else {
            return;
        };
        if !self.imp().selection_active.get() {
            // Keep the cursor where it is, draw nothing.
            return;
        }

        // The look is carried by a class rather than `:focus`; see the note on
        // `.lb-item--selected` in theme.rs. Every other item has it taken off,
        // so "exactly one item is selected" is true by construction rather
        // than by the pseudo-class happening to be exclusive.
        for stack in self.imp().stacks.borrow().iter() {
            for other in &stack.items {
                if other != &item {
                    other.remove_css_class(SELECTED_CLASS);
                }
            }
        }
        item.add_css_class(SELECTED_CLASS);

        // Still grabbed, even though nothing is drawn from it: it is what
        // makes the item reachable to a screen reader and keeps GTK's own idea
        // of the focus chain in step with ours.
        item.grab_focus();

        self.scroll_to_cursor();
        self.update_scroll_chips();
    }

    fn focused_item(&self) -> Option<LauncherItem> {
        let (s, r) = self.imp().cursor.get();
        let stacks = self.imp().stacks.borrow();
        stacks.get(s).and_then(|st| st.items.get(r)).cloned()
    }

    fn focused_entry_id(&self) -> Option<EntryId> {
        self.focused_item().and_then(|i| i.entry_id())
    }

    /// Put the focus somewhere sensible after a rebuild.
    ///
    /// In order of preference: where the child was, then what they last
    /// launched, then the first item that is actually launchable, then simply
    /// the first item — because the rule is that *something* is always focused.
    fn restore_focus(&self, previous: Option<EntryId>) {
        let imp = self.imp();
        let stacks = imp.stacks.borrow();
        if stacks.is_empty() {
            imp.cursor.set((0, 0));
            return;
        }

        let find = |wanted: &EntryId| -> Option<(usize, usize)> {
            stacks.iter().enumerate().find_map(|(s, stack)| {
                stack
                    .items
                    .iter()
                    .position(|i| i.entry_id().as_ref() == Some(wanted))
                    .map(|r| (s, r))
            })
        };

        let target = previous
            .as_ref()
            .and_then(&find)
            .or_else(|| imp.last_launched.borrow().as_ref().and_then(&find))
            .or_else(|| {
                stacks.iter().enumerate().find_map(|(s, stack)| {
                    stack
                        .items
                        .iter()
                        .position(LauncherItem::is_launchable)
                        .map(|r| (s, r))
                })
            })
            .unwrap_or((0, 0));

        drop(stacks);
        imp.cursor.set(target);
        self.focus_cursor();
    }

    /// Scroll the selection into view.
    ///
    /// Two cases, and the second is not the child's:
    ///
    /// * A compartment that **fits** the viewport scrolls as a whole, aligned
    ///   to the left margin. That is the brief's rule, and it is what keeps a
    ///   category from being half on screen.
    /// * A compartment **wider** than the viewport cannot be aligned to
    ///   anything useful, so the *item* is what gets scrolled to, by the least
    ///   amount that brings it fully into view. Administrator mode's picker is
    ///   one compartment holding every application on the host, so this is the
    ///   case it lives in permanently.
    fn scroll_to_cursor(&self) {
        // The scroll that follows a rebuild has nothing to animate from — the
        // row has only just appeared — so it lands instantly. Every later one
        // eases.
        let animate = !self.imp().pending_scroll.get();
        let imp = self.imp();
        let adj = imp.scroller.hadjustment();
        let page = adj.page_size();
        if page <= 0.0 {
            return;
        }

        let (s, _) = imp.cursor.get();
        let Some(well) = imp
            .stacks
            .borrow()
            .get(s)
            .map(|stack| stack.compartment.clone())
        else {
            return;
        };

        let alloc = well.allocation();
        if (alloc.width() as f64) <= page {
            let left = alloc.x() as f64;
            let right = left + alloc.width() as f64;
            if left < adj.value() || right > adj.value() + page {
                self.scroll_to(left, animate);
            }
            return;
        }

        let Some(item) = self.focused_item() else {
            return;
        };
        let Some((x, _)) = item.translate_coordinates(&imp.row, 0.0, 0.0) else {
            return;
        };
        // Keep the selection clear of the fades, or the item the child is on
        // would sit half-dissolved under one.
        let margin = theme::px(EDGE_MARGIN, imp.scale.get()) as f64;
        let left = x - margin;
        let right = x + item.width() as f64 + margin;
        if left < adj.value() {
            self.scroll_to(left.max(0.0), animate);
        } else if right > adj.value() + page {
            self.scroll_to(right - page, animate);
        }
    }

    /// Move the row to `target`, easing unless told not to.
    fn scroll_to(&self, target: f64, animate: bool) {
        if animate {
            self.animate_scroll_to(target);
            return;
        }
        let adj = self.imp().scroller.hadjustment();
        let upper = (adj.upper() - adj.page_size()).max(0.0);
        adj.set_value(target.clamp(0.0, upper));
    }

    /// Ease the row to `target` instead of teleporting there (#208 review).
    ///
    /// A jump gives no sense of which way the row went or how far, which on a
    /// touchscreen is the difference between "it moved" and "something
    /// happened". Retargeting mid-flight is fine: the next call reads the
    /// adjustment where it currently *is* and starts again from there, so
    /// holding a direction runs the row along smoothly rather than stuttering
    /// between finished animations.
    fn animate_scroll_to(&self, target: f64) {
        let adj = self.imp().scroller.hadjustment();
        let upper = (adj.upper() - adj.page_size()).max(0.0);
        let target = target.clamp(0.0, upper);
        let from = adj.value();
        if (target - from).abs() < 1.0 {
            return;
        }

        // Bump the generation so any animation already running stands down.
        let generation = self.imp().scroll_generation.get().wrapping_add(1);
        self.imp().scroll_generation.set(generation);

        let start = std::time::Instant::now();
        let field = self.downgrade();
        self.add_tick_callback(move |_, _| {
            let Some(field) = field.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if field.imp().scroll_generation.get() != generation {
                return glib::ControlFlow::Break;
            }
            let adj = field.imp().scroller.hadjustment();
            let t = start.elapsed().as_millis() as f64 / SCROLL_MS as f64;
            if t >= 1.0 {
                adj.set_value(target);
                return glib::ControlFlow::Break;
            }
            // Ease out: quick away from the old position, gentle into the new.
            let eased = 1.0 - (1.0 - t).powi(3);
            adj.set_value(from + (target - from) * eased);
            glib::ControlFlow::Continue
        });
    }

    /// Push the row along when the child steers past either end.
    fn nudge(&self, dx: i32) {
        let adj = self.imp().scroller.hadjustment();
        self.animate_scroll_to(
            adj.value() + scroll_step(adj.page_size(), self.imp().scale.get()) * dx as f64,
        );
    }

    /// Keep each compartment's name and badge on screen while the compartment
    /// itself scrolls past (#208 review).
    ///
    /// A category wider than the viewport used to take its own title off the
    /// left with it, leaving a screenful of activities belonging to nothing
    /// visible. The header now slides along inside its own compartment: pinned
    /// to the left edge of whatever part of that compartment is showing, and
    /// never further right than the compartment's own end, so it always reads
    /// as belonging to the well it is sitting in rather than floating over the
    /// row.
    ///
    /// Administrator mode is the case this matters most in — one compartment
    /// holding every installed application, whose title would otherwise be
    /// gone after the first swipe.
    fn slide_headers(&self) {
        let imp = self.imp();
        let adj = imp.scroller.hadjustment();
        let page = adj.page_size();
        if page <= 0.0 {
            return;
        }
        let view_left = adj.value();
        let view_right = view_left + page;
        // Clear of the fades, or a title would sit dissolving under the very
        // edge it is trying to stay ahead of.
        let inset = theme::px(EDGE_MARGIN, imp.scale.get()) as f64;
        // How close to its own compartment's ends the header may get: its
        // border and padding, and nothing more. The fade inset is about the
        // *screen* edge and has no business shrinking the room inside a
        // compartment — using it here left a narrow category almost no travel
        // at all, which looked exactly like the slide not working.
        let edge = theme::px(COMPARTMENT_EDGE, imp.scale.get()) as f64;

        for header in imp.headers.borrow().iter() {
            let well = header.well.allocation();
            let well_left = well.x() as f64;
            let well_right = well_left + well.width() as f64;

            // Positions come from `translate_coordinates` rather than from the
            // allocation, because an allocation is relative to the parent and
            // these need to be in the row's coordinates — the same ones the
            // adjustment counts in. Widths come from `measure`, not from the
            // allocation either: the name's bin is stretched across the whole
            // compartment to push the badge to the far end, so its *allocated*
            // width is the compartment's, and clamping against that would hold
            // every header still. (It did. That was the bug.)
            if let Some((x, _)) = header.name.translate_coordinates(&imp.row, 0.0, 0.0) {
                let (_, width, _, _) = header.name.measure(gtk4::Orientation::Horizontal, -1);
                header.name.set_offset(leading_offset(
                    view_left + inset,
                    x,
                    width as f64,
                    well_right - edge,
                ));
            }

            if let Some(badge) = &header.badge
                && let Some((x, _)) = badge.translate_coordinates(&imp.row, 0.0, 0.0)
            {
                let (_, width, _, _) = badge.measure(gtk4::Orientation::Horizontal, -1);
                badge.set_offset(trailing_offset(
                    view_right - inset,
                    x,
                    width as f64,
                    well_left + edge,
                ));
            }
        }
    }

    /// Keep the chevron chips honest: each shows only while the row really does
    /// continue past that edge.
    fn update_scroll_chips(&self) {
        let imp = self.imp();
        let adj = imp.scroller.hadjustment();
        // A pixel of slack: floating-point adjustment values land a hair off
        // their bounds, and a chip that never quite goes away is worse than one
        // that disappears a pixel early.
        let more_left = adj.value() > 1.0;
        let more_right = adj.value() + adj.page_size() < adj.upper() - 1.0;
        imp.reveal_left.set_reveal_child(more_left);
        imp.reveal_right.set_reveal_child(more_right);
        // The fade and its chip say the same thing, so they appear together.
        imp.fade_left.set_visible(more_left);
        imp.fade_right.set_visible(more_right);
    }

    /// Follow the adjustment, so the chips are right after a pointer or touch
    /// drag as well as after a D-pad move.
    fn wire_scroll_chips(&self) {
        let adj = self.imp().scroller.hadjustment();
        let field = self.downgrade();
        adj.connect_value_changed(move |_| {
            if let Some(field) = field.upgrade() {
                field.update_scroll_chips();
                field.slide_headers();
            }
        });
        let field = self.downgrade();
        adj.connect_changed(move |adj| {
            let Some(field) = field.upgrade() else {
                return;
            };
            // The adjustment learns its page size and extent during
            // allocation, which is the first moment a scroll can mean
            // anything. Take the one the rebuild could not do.
            if field.imp().pending_scroll.get() && adj.page_size() > 0.0 {
                field.imp().pending_scroll.set(false);
                field.scroll_to_cursor();
            }
            field.update_scroll_chips();
            field.slide_headers();
        });

        for (button, dx) in [
            (self.imp().more_left.clone(), -1),
            (self.imp().more_right.clone(), 1),
        ] {
            let field = self.downgrade();
            button.connect_clicked(move |_| {
                if let Some(field) = field.upgrade() {
                    field.nudge(dx);
                }
            });
        }
    }
}

impl Default for LauncherField {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lunchbox_util::GroupId;
    use std::time::Duration;

    fn entry(id: &str, group: Option<&str>) -> EntryView {
        EntryView {
            entry_id: EntryId::new(id),
            label: id.into(),
            icon_ref: None,
            kind_tag: lunchbox_api::EntryKindTag::Process,
            enabled: true,
            group: group.map(GroupId::new),
            reasons: vec![],
            tokens: None,
            earns_tokens: false,
            max_run_if_started_now: None,
        }
    }

    fn group(id: &str, label: &str) -> GroupView {
        GroupView {
            group_id: GroupId::new(id),
            label: label.into(),
            member_ids: vec![],
            enabled: true,
            reasons: vec![],
            used_today: Duration::ZERO,
            daily_quota: None,
            max_run_if_started_now: None,
            tokens: None,
            window_closes_at: None,
            earns_tokens: false,
        }
    }

    fn shape(cats: &[(String, Option<&GroupView>, Vec<EntryView>)]) -> Vec<(String, Vec<String>)> {
        cats.iter()
            .map(|(label, _, members)| {
                (
                    label.clone(),
                    members
                        .iter()
                        .map(|e| e.entry_id.as_str().to_string())
                        .collect(),
                )
            })
            .collect()
    }

    // The geometry these use is the real thing, measured off the running
    // launcher: a compartment starts at 40 with 4px of border and 16px of
    // padding, so its header content begins at 60. The first version of these
    // tests invented a width instead, and passed while the headers stood
    // perfectly still on a device — the formula was right and the numbers I
    // fed it were not.
    const INSET: f64 = 56.0;
    const NAME_X: f64 = 60.0;
    const NAME_W: f64 = 120.0;

    /// A compartment fully on screen keeps its name where it was laid out.
    #[test]
    fn a_visible_compartment_does_not_move_its_name() {
        assert_eq!(leading_offset(0.0 + INSET, NAME_X, NAME_W, 2984.0), 0.0);
        // And still nothing once scrolled, while the compartment's own start
        // is ahead of the viewport.
        assert_eq!(leading_offset(20.0 + INSET, 400.0, NAME_W, 2984.0), 0.0);
    }

    /// Once the viewport cuts into a compartment, the name follows the cut.
    #[test]
    fn a_name_follows_the_viewport_into_its_compartment() {
        // Scrolled to 500: the name sits 56 past the screen edge, which is
        // 496 further right than where it was laid out.
        assert_eq!(leading_offset(500.0 + INSET, NAME_X, NAME_W, 2984.0), 496.0);
    }

    /// And stops at its own compartment's end rather than running into the
    /// next category's.
    #[test]
    fn a_name_stops_at_the_end_of_its_own_compartment() {
        // A 400-wide well ending at 440, so the name may reach 384 - 120.
        let stop = 384.0 - NAME_W;
        assert_eq!(leading_offset(5000.0, NAME_X, NAME_W, 384.0), stop - NAME_X);
    }

    /// A name with nowhere to go must not be dragged backwards trying.
    #[test]
    fn a_name_wider_than_its_compartment_stays_put() {
        assert_eq!(leading_offset(5000.0, NAME_X, 400.0, 384.0), 0.0);
    }

    /// The badge holds the *other* edge: it is at the far end of the header,
    /// so it slides left to stay on screen while the compartment's tail is
    /// still off it.
    #[test]
    fn a_badge_holds_the_right_of_what_is_visible() {
        // A 3000-wide compartment, badge laid out at 2960, 1280 of viewport.
        let offset = trailing_offset(1280.0 - INSET, 2960.0, 60.0, 96.0);
        assert_eq!(2960.0 + offset, 1280.0 - INSET - 60.0);
    }

    /// A compartment that fits leaves its badge alone.
    #[test]
    fn a_visible_compartment_does_not_move_its_badge() {
        assert_eq!(trailing_offset(1224.0, 360.0, 60.0, 96.0), 0.0);
    }

    /// And the badge never crosses its own compartment's start.
    #[test]
    fn a_badge_stops_at_the_start_of_its_own_compartment() {
        let offset = trailing_offset(44.0, 360.0, 60.0, 96.0);
        assert_eq!(360.0 + offset, 96.0);
    }

    /// A chevron press moves most of a screen, not one activity.
    #[test]
    fn a_chevron_moves_a_screenful() {
        // 1280 of viewport at 1x keeps one 160px item as overlap.
        assert_eq!(scroll_step(1280.0, 1.0), 1120.0);
        // Scaled up, the overlap scales with everything else.
        assert_eq!(scroll_step(1920.0, 1.5), 1920.0 - 240.0);
        // A viewport narrower than one item still moves, and never backwards.
        assert!(scroll_step(100.0, 1.0) > 0.0);
    }

    #[test]
    fn categories_come_out_in_config_order() {
        let groups = vec![group("books", "Books"), group("play", "Play")];
        let entries = vec![
            entry("celeste", Some("play")),
            entry("hobbit", Some("books")),
        ];
        assert_eq!(
            shape(&categorise(&entries, &groups)),
            [
                ("Books".to_string(), vec!["hobbit".to_string()]),
                ("Play".to_string(), vec!["celeste".to_string()]),
            ],
            "the row follows the order the groups are declared in, not the \
             order the entries happen to arrive in"
        );
    }

    #[test]
    fn members_keep_their_own_config_order() {
        let groups = vec![group("play", "Play")];
        let entries = vec![
            entry("celeste", Some("play")),
            entry("hike", Some("play")),
            entry("firered", Some("play")),
        ];
        assert_eq!(
            shape(&categorise(&entries, &groups))[0].1,
            ["celeste", "hike", "firered"]
        );
    }

    #[test]
    fn ungrouped_entries_fall_into_a_trailing_category() {
        let groups = vec![group("play", "Play")];
        let entries = vec![entry("krita", None), entry("celeste", Some("play"))];
        let cats = shape(&categorise(&entries, &groups));
        assert_eq!(cats.len(), 2);
        assert_eq!(cats[0].0, "Play");
        assert_eq!(
            cats[1],
            (UNGROUPED_LABEL.to_string(), vec!["krita".to_string()]),
            "the catch-all is always last"
        );
    }

    #[test]
    fn a_category_with_nothing_to_show_is_dropped_entirely() {
        // An empty well says "there is something here" when there isn't.
        let groups = vec![group("books", "Books"), group("play", "Play")];
        let entries = vec![entry("celeste", Some("play"))];
        let cats = shape(&categorise(&entries, &groups));
        assert_eq!(cats.len(), 1);
        assert_eq!(cats[0].0, "Play");
    }

    #[test]
    fn no_categories_at_all_is_one_plain_tin() {
        let entries = vec![entry("krita", None), entry("lofi", None)];
        let cats = shape(&categorise(&entries, &[]));
        assert_eq!(
            cats,
            [(
                UNGROUPED_LABEL.to_string(),
                vec!["krita".to_string(), "lofi".to_string()]
            )],
            "a config that declares no groups still gets a tin"
        );
    }

    #[test]
    fn an_entry_whose_category_was_not_reported_is_not_lost() {
        // Dropping it would be the one failure the child cannot work around.
        let groups = vec![group("play", "Play")];
        let entries = vec![entry("orphan", Some("vanished"))];
        let cats = shape(&categorise(&entries, &groups));
        assert_eq!(
            cats,
            [(UNGROUPED_LABEL.to_string(), vec!["orphan".to_string()])]
        );
    }

    #[test]
    fn locked_members_stay_but_broken_ones_do_not() {
        let groups = vec![group("play", "Play")];
        let mut cooling = entry("celeste", Some("play"));
        cooling.enabled = false;
        cooling.reasons = vec![lunchbox_api::ReasonCode::CooldownActive {
            available_at: lunchbox_util::now(),
        }];
        let mut broken = entry("steam-thing", Some("play"));
        broken.enabled = false;
        broken.reasons = vec![lunchbox_api::ReasonCode::UnsupportedKind {
            kind: lunchbox_api::EntryKindTag::Steam,
        }];

        let cats = shape(&categorise(&[cooling, broken], &groups));
        assert_eq!(
            cats,
            [("Play".to_string(), vec!["celeste".to_string()])],
            "a cooling activity keeps its place so its badge can explain \
             itself; one this host cannot run does not"
        );
    }

    #[test]
    fn a_category_left_with_only_broken_members_disappears() {
        let groups = vec![group("play", "Play")];
        let mut broken = entry("steam-thing", Some("play"));
        broken.enabled = false;
        broken.reasons = vec![lunchbox_api::ReasonCode::ProtectionUnavailable];
        assert!(categorise(&[broken], &groups).is_empty());
    }
}
