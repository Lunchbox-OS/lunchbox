//! The field: the row of compartments the child actually looks at.
//!
//! Replaces the flat `FlowBox` grid on the home screen. (`grid.rs` keeps the
//! flow box, because administrator mode's app picker is a searchable list of
//! everything installed, which is a different problem and deliberately not
//! branded as the child's tin.)
//!
//! Two rules from the branding drive the whole of this file:
//!
//! 1. **Never scroll vertically.** Categories are columns; a category with more
//!    than three members grows *wider*, and if the row overflows, the row
//!    scrolls sideways.
//! 2. **Exactly one item is focused whenever the field is showing**, and a
//!    locked item can be focused but not pressed — being able to reach it is
//!    how the child reads the badge that says what would unlock it.

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

mod imp {
    use super::*;

    type LaunchCallback = Rc<RefCell<Option<Box<dyn Fn(EntryId) + 'static>>>>;

    pub struct LauncherField {
        pub scroller: gtk4::ScrolledWindow,
        pub row: gtk4::Box,
        pub more_left: gtk4::Button,
        pub more_right: gtk4::Button,
        /// The soft edge the row disappears under, one per side. Shown with
        /// the chip above it, and on the same condition.
        pub fade_left: gtk4::Box,
        pub fade_right: gtk4::Box,
        pub stacks: RefCell<Vec<Stack>>,
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
                fade_left: gtk4::Box::new(gtk4::Orientation::Vertical, 0),
                fade_right: gtk4::Box::new(gtk4::Orientation::Vertical, 0),
                stacks: RefCell::new(Vec::new()),
                cursor: Cell::new((0, 0)),
                on_launch: Rc::new(RefCell::new(None)),
                last_launched: RefCell::new(None),
                scale: Cell::new(1.0),
                pending_scroll: Cell::new(false),
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
                button.set_visible(false);
                // Not a tab stop: it is a signpost for a row that the D-pad
                // already scrolls, not a control to land on.
                button.set_can_focus(false);
            }

            let overlay = gtk4::Overlay::new();
            overlay.set_child(Some(&self.scroller));
            overlay.add_overlay(&self.fade_left);
            overlay.add_overlay(&self.fade_right);
            overlay.add_overlay(&self.more_left);
            overlay.add_overlay(&self.more_right);
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
        obj
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
        for (label, group, members) in categories {
            let built = compartment::build(&label, group, members, scale);
            imp.row.append(&built.widget);
            for items in built.stacks {
                stacks.push(Stack {
                    compartment: built.widget.clone(),
                    items,
                });
            }
        }

        for (s, stack) in stacks.iter().enumerate() {
            for (r, item) in stack.items.iter().enumerate() {
                self.wire_item(item, s, r);
            }
        }
        *imp.stacks.borrow_mut() = stacks;

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
        if let Some(item) = self.focused_item() {
            self.press(&item);
        }
    }

    /// Move the focus. `dx` steps between stacks, `dy` within one.
    pub fn move_selection(&self, dx: i32, dy: i32) {
        let imp = self.imp();
        let stacks = imp.stacks.borrow();
        if stacks.is_empty() {
            return;
        }
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
    fn focus_cursor(&self) {
        let Some(item) = self.focused_item() else {
            return;
        };

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
                adj.set_value(left);
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
            adj.set_value(left.max(0.0));
        } else if right > adj.value() + page {
            adj.set_value(right - page);
        }
    }

    /// Push the row along when the child steers past either end.
    fn nudge(&self, dx: i32) {
        let adj = self.imp().scroller.hadjustment();
        let step = theme::px(theme::ITEM_W, self.imp().scale.get()) as f64;
        adj.set_value(
            (adj.value() + step * dx as f64).clamp(0.0, (adj.upper() - adj.page_size()).max(0.0)),
        );
        self.update_scroll_chips();
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
        imp.more_left.set_visible(more_left);
        imp.more_right.set_visible(more_right);
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
