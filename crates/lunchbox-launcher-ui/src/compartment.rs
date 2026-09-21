//! One category: a cream compartment sunk into the enamel field.
//!
//! A compartment is a *well*, not a card. That is the whole of its visual
//! character and the reason it carries inset shadows and no drop shadow — the
//! CSS in `theme.rs` does the drawing; this file decides what goes in it.
//!
//! Its contents, top to bottom: the category's name and (at most) one badge,
//! then the items — dealt across in reading order into columns that spill to
//! the right — then, only when the category actually shuts today, a hairline
//! floor and the closing time.
//!
//! How tall a stack may be is [`rows_that_fit`], and it is *measured* rather
//! than decided here. See that function for why.

use gtk4::prelude::*;
use lunchbox_api::{EntryView, GroupView, ReasonCode};

use crate::badge::Badge;
use crate::item::LauncherItem;
use crate::theme;

/// Diameter of the clock face on a compartment's floor, unscaled.
///
/// The concept draws it a shade larger than the type beside it, so it reads as
/// a picture rather than as one more glyph in the line. Derived from the
/// footer's own size rather than written down, so the two move together.
const FLOOR_CLOCK_PX: i32 = theme::tokens::FOOTER_FONT_PX + 2;

/// Vertical gap between items in a stack, unscaled.
///
/// Named because two things need it and they must not disagree: the column
/// that lays the items out, and the arithmetic in [`rows_that_fit`] that works
/// out how many of them there is room for.
const ROW_GAP: i32 = 8;

/// Horizontal gap between the stacks inside one compartment, unscaled.
const STACK_GAP: i32 = 16;

/// The narrowest a compartment may be, counted in item columns.
///
/// A category holding one stack is otherwise exactly one item wide, and the
/// header has to fit a name *and* a badge pushed to the far end of that same
/// line. The label does not ellipsize, so it wins: the compartment stretches
/// to whatever the words need, and a row of them ends up at as many different
/// widths as there are category names — which is the thing pushing the badges
/// to the far end was meant to fix in the first place. Two columns is the
/// floor whatever the category holds, so the header always has somewhere to
/// put both.
const MIN_COLUMNS: i32 = 2;

/// The narrowest the items area may be, in real pixels: [`MIN_COLUMNS`] cells
/// of `item_w` with the stack gaps between them.
///
/// Takes the cell width rather than reading it off the tokens, because a row
/// that overflows its screen by a sliver squishes its cells to fit
/// (`LauncherField::squish_to_fit`) and this floor has to squish with them, or
/// a one-stack category would hold the row open on its own.
fn columns_floor(item_w: i32, scale: f64) -> i32 {
    MIN_COLUMNS * item_w + (MIN_COLUMNS - 1) * theme::px(STACK_GAP, scale)
}

/// A category on screen, and the items it holds in the order they are drawn.
pub struct Compartment {
    /// The well itself, to put in the row.
    pub widget: gtk4::Widget,
    /// The name, in the bin that slides it along so it stays on screen while a
    /// wide compartment scrolls past (#208 review). The field drives it.
    pub name: crate::offset::OffsetBin,
    /// The category's badge, if it has one, in a bin of its own — it anchors
    /// to the opposite edge from the name.
    pub badge: Option<crate::offset::OffsetBin>,
    /// The items, grouped into the columns they were laid out in. The field's
    /// D-pad model is built on exactly this shape: left/right move between
    /// stacks, up/down within one. Note that the *filling* runs the other way
    /// — see `split_into_stacks`.
    pub stacks: Vec<Vec<LauncherItem>>,
    /// The box the stacks sit in, kept only so that [`Compartment::squish`]
    /// can bring its two-column floor down with the cells.
    columns: gtk4::Box,
}

impl Compartment {
    /// Narrow every cell in this compartment to `item_w`, floor included.
    ///
    /// Only ever called to rescue a row that misses fitting its screen by less
    /// than half a cell — see `LauncherField::squish_to_fit`.
    pub fn squish(&self, item_w: i32, scale: f64) {
        for column in &self.stacks {
            for item in column {
                item.set_width_cap(item_w);
            }
        }
        self.columns
            .set_size_request(columns_floor(item_w, scale), -1);
    }
}

/// Build the compartment for a category.
///
/// `group` is `None` for the implicit trailing category that collects entries
/// belonging to no group; it has a name but no schedule and no shared gate, so
/// it never carries a badge or a floor.
///
/// `rows` is how tall a stack may be before the category spills into another
/// one beside it. The field measures it once per layout ([`rows_that_fit`])
/// and hands every compartment the same number, so a row of them stays a tin
/// with sections rather than a skyline.
pub fn build(
    label: &str,
    group: Option<&GroupView>,
    entries: Vec<EntryView>,
    scale: f64,
    rows: usize,
) -> Compartment {
    let well = new_well();

    let category = group.and_then(category_badge);
    let (header, name_bin, badge_bin) = build_header(label, category.as_ref(), scale);
    well.append(&header);

    // -------------------------------------------------------------- items
    let stacks = split_into_stacks(entries, rows);
    let columns = gtk4::Box::new(gtk4::Orientation::Horizontal, theme::px(STACK_GAP, scale));
    columns.set_vexpand(true);
    columns.set_valign(gtk4::Align::Start);
    // The floor on the compartment's width, set here rather than on the well
    // so the compartment's own padding and border are still added on top of it
    // — the stylesheet keeps those, and this keeps the item geometry.
    columns.set_size_request(columns_floor(theme::px(theme::ITEM_W, scale), scale), -1);

    let mut built: Vec<Vec<LauncherItem>> = Vec::new();
    let now = lunchbox_util::now();
    for stack in stacks {
        let column = new_column(scale);
        let mut items = Vec::new();
        for entry in stack {
            let item = LauncherItem::new();
            let badge = item_badge(&entry, category.as_ref(), now);
            item.set_entry(entry, scale, badge);
            column.append(&item);
            items.push(item);
        }
        columns.append(&column);
        built.push(items);
    }
    well.append(&columns);

    // -------------------------------------------------------------- floor
    // Only a category on a schedule has anything to say down here; an
    // always-available one gets no floor at all rather than an empty rule.
    if let Some(schedule) = group.and_then(schedule_line) {
        well.append(&build_floor(schedule, scale));
    }

    Compartment {
        widget: well.upcast(),
        name: name_bin,
        badge: badge_bin,
        stacks: built,
        columns,
    }
}

/// The sunk well itself, empty.
fn new_well() -> gtk4::Box {
    let well = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    well.add_css_class("lb-compartment");
    // Every compartment is the full height of the field, so the row reads as
    // one tin rather than a skyline. Width is what grows when a category holds
    // more than one stack's worth of members (§3 of the brief).
    well.set_valign(gtk4::Align::Fill);
    well.set_vexpand(true);
    // Explicitly *not* horizontally expanding. GTK computes expansion from a
    // widget's children unless the widget states its own, and the header's name
    // label expands (to push the badge to the far end). Left to propagate, that
    // reaches the row and every compartment stretches to fill the viewport —
    // which would make a category's width depend on how much screen is spare
    // rather than on how many stacks it holds. Setting it here stops the
    // propagation without stopping the header from doing its job.
    well.set_hexpand(false);
    well
}

/// One stack, empty.
fn new_column(scale: f64) -> gtk4::Box {
    let column = gtk4::Box::new(gtk4::Orientation::Vertical, theme::px(ROW_GAP, scale));
    column.set_valign(gtk4::Align::Start);
    column
}

/// The compartment's top line: the category's name, and at most one badge
/// pushed to the far end of it.
fn build_header(
    label: &str,
    badge: Option<&Badge>,
    scale: f64,
) -> (
    gtk4::Box,
    crate::offset::OffsetBin,
    Option<crate::offset::OffsetBin>,
) {
    let header = gtk4::Box::new(gtk4::Orientation::Horizontal, theme::px(10, scale));
    header.add_css_class("lb-compartment__header");

    let name = gtk4::Label::new(Some(label));
    name.add_css_class("lb-compartment__name");
    name.set_xalign(0.0);
    // Each half of the header slides independently, because they anchor to
    // opposite edges when the compartment is wider than the screen: the name
    // holds the left of what is visible, the badge the right. One bin around
    // the pair could only move them together, which would carry the badge off
    // the compartment's far end.
    let name_bin = crate::offset::OffsetBin::around(&name);
    // The name's bin takes the slack, which pushes the badge to the far end of
    // the header instead of leaving it tucked against the name.
    //
    // A deliberate departure from the mockup, which sets the badge immediately
    // after the category name. Across a row of compartments of different widths
    // that puts every badge at a different offset; at the end they line up with
    // each compartment's right edge, and the eye can run down them.
    name_bin.set_hexpand(true);
    name_bin.set_halign(gtk4::Align::Fill);
    header.append(&name_bin);

    let badge_bin = badge.map(|badge| {
        let bin = crate::offset::OffsetBin::around(&badge.widget(scale));
        bin.set_halign(gtk4::Align::End);
        header.append(&bin);
        bin
    });

    (header, name_bin, badge_bin)
}

/// The compartment's bottom line: a hairline, a clock face pointing at the
/// hour the category shuts, and that hour in words.
fn build_floor(schedule: Schedule, scale: f64) -> gtk4::Box {
    let floor = gtk4::Box::new(gtk4::Orientation::Horizontal, theme::px(6, scale));
    floor.add_css_class("lb-compartment__floor");
    floor.set_valign(gtk4::Align::End);
    floor.set_vexpand(true);

    // The face points at the hour being talked about, not at the time it is.
    // That is the whole reason it is worth drawing rather than writing out: a
    // child who cannot yet read "6:00 PM" can still see where the hand is
    // going to be.
    let face = lunchbox_widgets::ClockFace::at(schedule.at, theme::px(FLOOR_CLOCK_PX, scale));
    face.add_css_class("lb-compartment__clock");
    face.set_valign(gtk4::Align::Center);
    floor.append(&face);

    let label = gtk4::Label::new(Some(&schedule.text()));
    label.add_css_class("lb-compartment__schedule");
    label.set_xalign(0.0);
    floor.append(&label);
    floor
}

/// The one badge a category may wear.
///
/// At most one, and earning wins: a category that is both a source for some
/// gate and gated itself is rare, and "you can earn here" is the more useful
/// half to a child looking for something to do.
fn category_badge(group: &GroupView) -> Option<Badge> {
    if group.earns_tokens {
        return Some(Badge::Earn);
    }
    group.tokens.as_ref().and_then(Badge::for_tokens)
}

/// The badge one item wears, given what its category is already saying.
///
/// The nearest obstacle wins, because that is the one the child has to clear
/// first: a cooldown before a gate, a gate before an invitation to earn.
///
/// The last rule is the one that is about the *field* rather than the item. An
/// activity that earns sits in a category that also earns, so the category
/// already carries the pill; repeating it on every member turns one piece of
/// information into five and buries the items that have something of their own
/// to say.
fn item_badge(
    entry: &EntryView,
    category: Option<&Badge>,
    now: chrono::DateTime<chrono::Local>,
) -> Option<Badge> {
    if let Some(wait) = Badge::for_cooldown(&entry.reasons, now) {
        return Some(wait);
    }
    if let Some(gate) = entry.tokens.as_ref().and_then(Badge::for_tokens) {
        return Some(gate);
    }
    if entry.earns_tokens && category != Some(&Badge::Earn) {
        return Some(Badge::Earn);
    }
    None
}

/// What the compartment floor has to say about this category's hours.
///
/// One hour and one word, which is all the floor has room for and all the
/// question needs: a category is either open and about to shut, or shut and
/// about to open.
pub struct Schedule {
    /// The hour the face points at.
    at: chrono::DateTime<chrono::Local>,
    /// Whether that hour is when the category opens rather than shuts.
    opens: bool,
}

impl Schedule {
    fn text(&self) -> String {
        let verb = if self.opens { "Opens" } else { "Until" };
        format!("{verb} {}", format_clock(self.at))
    }
}

/// What this category's floor says, if it says anything.
///
/// A category with no schedule at all has no floor: an empty rule under an
/// always-available category is a line about nothing. One that is shut says
/// when it opens, and one that is open says when it shuts — the shut half is
/// the bedtime screen the brief left to be designed (§9), and it was blocked
/// until the engine started working out `next_window_start`.
///
/// The shut case reads its hour out of the *reason* rather than off the view,
/// because that is where the engine puts it. A category shut for some other
/// reason — its quota spent, a cooldown running — has no hour to give, and
/// keeps its floor quiet rather than inventing one.
fn schedule_line(group: &GroupView) -> Option<Schedule> {
    if let Some(at) = next_window_start(&group.reasons) {
        return Some(Schedule { at, opens: true });
    }
    group
        .window_closes_at
        .map(|at| Schedule { at, opens: false })
}

/// The hour an `OutsideTimeWindow` among these reasons says the category comes
/// back, if one of them says so.
fn next_window_start(reasons: &[ReasonCode]) -> Option<chrono::DateTime<chrono::Local>> {
    reasons.iter().find_map(|reason| match reason {
        ReasonCode::OutsideTimeWindow { next_window_start } => *next_window_start,
        _ => None,
    })
}

/// Lay a category's members out in columns at most `rows` tall, in config
/// order. Width grows; height never does.
///
/// **Filled across, returned down.** The members are dealt into the columns in
/// reading order — the top row left to right, then the row under it — so a
/// category that does not fill its compartment leaves the gap along the
/// *bottom* rather than down the right-hand side. A compartment is never
/// narrower than [`MIN_COLUMNS`], so the alternative is a tin with an empty
/// column standing in it, which reads as a section of the lunchbox somebody
/// forgot to pack.
///
/// What comes back is still the *columns*, because that is the shape the
/// field's D-pad model is built on: left and right move between them, up and
/// down inside one. Only the dealing changed, not the geometry — a category
/// with seven members and room for three rows is three columns wide either
/// way.
fn split_into_stacks(entries: Vec<EntryView>, rows: usize) -> Vec<Vec<EntryView>> {
    if entries.is_empty() {
        return Vec::new();
    }
    let rows = rows.max(1);
    // Wide enough to hold them all, but never wider than the compartment's own
    // floor — and never wider than there are members to put in it, or the row
    // would carry a column with nothing in it for the selection to land on.
    let columns = entries
        .len()
        .div_ceil(rows)
        .max(MIN_COLUMNS as usize)
        .min(entries.len());

    let mut stacks = vec![Vec::new(); columns];
    for (i, entry) in entries.into_iter().enumerate() {
        stacks[i % columns].push(entry);
    }
    stacks
}

/// What the field's height means for the compartments in it.
pub struct Budget {
    /// The height a column of the field actually gets, once the field's own
    /// margins above and below the row are out of it. What
    /// [`pack_into_slots`] measures against.
    pub height: i32,
    /// Items one column of a compartment can hold.
    pub rows: usize,
}

/// Work out, for a field `field_height` px tall, how much room a column of it
/// has and how many items fit in one column of a compartment.
///
/// The design hands down a fixed three rows (`space.rows` in `tokens.json`),
/// and three is what a 1280×720 screen has room for. But the launcher runs on
/// whatever the device has, and `scale_for` deliberately scales by the
/// *narrower* axis so the row never reflows — which means a screen taller than
/// 16:9 ends up with spare height under the compartments that nothing uses,
/// and a shorter one clips. So the number is worked out per layout instead,
/// and the token becomes the value used before the field has been measured.
///
/// **Measured, not calculated.** A compartment's vertical budget is the
/// field's own padding, the compartment's border and padding, a header, a
/// floor, and *n* item cells with gaps between them — and every one of those
/// numbers lives in the stylesheet. Asking GTK what they add up to is the only
/// answer that cannot drift from the CSS, and drift is exactly what went wrong
/// when this was three by construction: the item cell grew to 178px against
/// the 150px the geometry assumed, and the compartment clipped at 720p with
/// nothing to catch it.
///
/// The probe is the worst case on purpose — a header wearing a badge and a
/// floor, which not every category has — so one row count fits every
/// compartment in the field and they all agree on where their items start.
pub fn budget(field_height: i32, scale: f64) -> Budget {
    // Before the window knows its size there is nothing to measure against.
    // The first layout after startup is always a re-layout (see the field's
    // `last_state`), so this is the value for one frame at most.
    if field_height <= 0 {
        return Budget {
            height: 0,
            rows: theme::ROWS_PER_STACK,
        };
    }

    // `.lb-field` carries the margins above and below the row, so the probe
    // starts there rather than at the compartment: the padding is in the
    // stylesheet and this way it never has to be named twice.
    let field = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    field.add_css_class("lb-field");
    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    row.add_css_class("lb-field__row");
    field.append(&row);
    let measure = || field.measure(gtk4::Orientation::Vertical, -1).1;

    // Three measurements of the same probe, each adding one part: the field's
    // margins, then a compartment with nothing in it, then one item.
    let margins = measure();

    let well = new_well();
    let (header, ..) = build_header("Category", Some(&Badge::Earn), scale);
    well.append(&header);
    let columns = gtk4::Box::new(gtk4::Orientation::Horizontal, theme::px(STACK_GAP, scale));
    let column = new_column(scale);
    columns.append(&column);
    well.append(&columns);
    well.append(&build_floor(
        Schedule {
            at: lunchbox_util::now(),
            opens: false,
        },
        scale,
    ));
    row.append(&well);

    let chrome = measure() - margins;

    // Every item is the same height whatever its name — `item.rs` reserves two
    // lines and caps the label at two — so one probe stands for all of them.
    let probe = LauncherItem::new();
    probe.set_entry(probe_entry(), scale, Some(Badge::Earn));
    column.append(&probe);
    let cell = measure() - margins - chrome;

    let height = field_height - margins;
    Budget {
        height,
        rows: rows_in(height - chrome, cell, theme::px(ROW_GAP, scale)),
    }
}

/// Group the compartments into the field's columns, in config order.
///
/// Returns one range of compartment indices per column of the field. A column
/// takes as many compartments as will stand in it — a category only claims the
/// height its own items need, and two short ones in a tall field would
/// otherwise each waste most of a screen.
///
/// Greedy, and strictly in order, so that reading a column downwards and then
/// moving right reads the categories in the order the configuration lists
/// them. A cleverer packing could fit more in (Books and Listen might pair
/// where Books and Learn do not), but it would do it by shuffling the
/// categories, and a home screen whose sections move around when one of them
/// gains an activity is worse than one with a gap in it.
///
/// A compartment too tall for the field at all still gets a column of its own
/// rather than being dropped: that is administrator mode's picker, which is
/// one compartment holding every application on the host.
pub fn pack_into_slots(heights: &[i32], available: i32, gap: i32) -> Vec<std::ops::Range<usize>> {
    let mut slots: Vec<std::ops::Range<usize>> = Vec::new();
    let mut start = 0;
    let mut used = 0;

    for (i, &height) in heights.iter().enumerate() {
        if i == start {
            used = height;
            continue;
        }
        let stacked = used + gap + height;
        if stacked > available {
            slots.push(start..i);
            start = i;
            used = height;
        } else {
            used = stacked;
        }
    }
    if start < heights.len() {
        slots.push(start..heights.len());
    }
    slots
}

/// How many `cell`-tall rows fit in `room` px with `gap` between them.
///
/// Split out of [`rows_that_fit`] because it is the only part of that function
/// a test can reach: the rest needs a display connection.
fn rows_in(room: i32, cell: i32, gap: i32) -> usize {
    if cell <= 0 {
        return theme::ROWS_PER_STACK;
    }
    // n cells and n-1 gaps fit in `room`, so lend the sum one more gap and the
    // division comes out whole.
    let n = (room + gap) / (cell + gap);
    // Never zero: a compartment with no room for even one item is a screen
    // this design cannot serve, and showing one clipped item says that far
    // better than showing an empty tin.
    n.max(1) as usize
}

/// A stand-in activity, for measuring a cell that is never shown.
fn probe_entry() -> EntryView {
    EntryView {
        entry_id: lunchbox_util::EntryId::new("__probe__"),
        label: "Probe".into(),
        icon_ref: None,
        kind_tag: lunchbox_api::EntryKindTag::Process,
        enabled: true,
        group: None,
        reasons: vec![],
        tokens: None,
        earns_tokens: false,
        max_run_if_started_now: None,
    }
}

/// A wall-clock time the way the compartment floor says it: "6:00 PM".
fn format_clock(at: chrono::DateTime<chrono::Local>) -> String {
    // `%-I` drops the leading zero, so it reads 6:00 PM rather than 06:00 PM.
    at.format("%-I:%M %p").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use lunchbox_util::{EntryId, GroupId};
    use std::time::Duration;

    fn entry(id: &str) -> EntryView {
        EntryView {
            entry_id: EntryId::new(id),
            label: id.into(),
            icon_ref: None,
            kind_tag: lunchbox_api::EntryKindTag::Process,
            enabled: true,
            group: None,
            reasons: vec![],
            tokens: None,
            earns_tokens: false,
            max_run_if_started_now: None,
        }
    }

    fn group() -> GroupView {
        GroupView {
            group_id: GroupId::new("attention-heavy"),
            label: "Games".into(),
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

    /// The stack height the design hands down, and what the tests below use
    /// unless they are about some other screen.
    const THREE: usize = 3;

    /// The columns, as lengths — the shape of the compartment, without the
    /// names getting in the way.
    fn shape(entries: usize, rows: usize) -> Vec<usize> {
        let members = (0..entries).map(|i| entry(&i.to_string())).collect();
        split_into_stacks(members, rows)
            .iter()
            .map(Vec::len)
            .collect()
    }

    /// The field's columns, as (first category, one past the last) — tuples
    /// rather than the ranges themselves only so that a one-column answer can
    /// be written down without tripping `single_range_in_vec_init`.
    fn packed(heights: &[i32], available: i32, gap: i32) -> Vec<(usize, usize)> {
        pack_into_slots(heights, available, gap)
            .into_iter()
            .map(|r| (r.start, r.end))
            .collect()
    }

    /// The members in the order they are read off the screen: across the top
    /// row, then the row under it.
    fn reading_order(stacks: &[Vec<EntryView>]) -> Vec<String> {
        let rows = stacks.iter().map(Vec::len).max().unwrap_or(0);
        (0..rows)
            .flat_map(|r| {
                stacks
                    .iter()
                    .filter_map(move |column| column.get(r))
                    .map(|e| e.entry_id.as_str().to_string())
            })
            .collect()
    }

    /// A category that does not fill its compartment leaves the gap along the
    /// bottom, not down the right-hand side: the compartment is two columns
    /// wide whatever it holds, and an empty column standing in it reads as a
    /// section somebody forgot to pack.
    #[test]
    fn a_short_category_is_dealt_across_the_columns() {
        assert_eq!(shape(3, THREE), [2, 1], "two on the top row, one under");
        assert_eq!(shape(2, THREE), [1, 1], "side by side, not stacked");
    }

    /// Two columns is the compartment's floor, but a column with nothing in it
    /// would be a dead stop for the D-pad, so one member still makes one.
    #[test]
    fn a_single_member_does_not_conjure_an_empty_column() {
        assert_eq!(shape(1, THREE), [1]);
    }

    #[test]
    fn a_full_row_of_columns_spills_downwards() {
        // The acceptance checklist: 4-6 members render double-wide, 7-9 triple.
        assert_eq!(shape(4, THREE), [2, 2], "4 members are double-wide");
        assert_eq!(shape(6, THREE), [3, 3]);
        assert_eq!(shape(7, THREE), [3, 2, 2], "7 members are triple-wide");
        assert_eq!(shape(9, THREE), [3, 3, 3]);
    }

    /// A screen with room for a taller stack makes the compartment narrower,
    /// down to the two-column floor — the same widths as before this was dealt
    /// across; only which member lands where changed.
    #[test]
    fn a_taller_screen_needs_fewer_columns() {
        assert_eq!(shape(12, 3).len(), 4);
        assert_eq!(shape(12, 4).len(), 3);
        assert_eq!(shape(12, 6).len(), 2);
        assert_eq!(shape(12, 12).len(), 2, "never narrower than the floor");
    }

    #[test]
    fn config_order_survives_the_split() {
        let stacks = split_into_stacks(
            vec![
                entry("first"),
                entry("second"),
                entry("third"),
                entry("fourth"),
            ],
            THREE,
        );
        assert_eq!(
            reading_order(&stacks),
            ["first", "second", "third", "fourth"],
            "config order is reading order: across, then down"
        );
    }

    /// `rows_that_fit` never answers zero, but the split must survive one
    /// anyway rather than dividing by it.
    #[test]
    fn a_stack_always_holds_something() {
        assert_eq!(shape(2, 0), [1, 1], "one item each, not a panic");
    }

    /// Two short categories share a column of the field; a third that would
    /// not fit starts the next one.
    #[test]
    fn short_compartments_stand_one_above_another() {
        // 300 high each, 20 of gap, in a field with 700 to give.
        assert_eq!(packed(&[300, 300, 300], 700, 20), [(0, 2), (2, 3)]);
        assert_eq!(packed(&[300, 300], 700, 20), [(0, 2)]);
        assert_eq!(packed(&[300, 300, 300], 1000, 20), [(0, 3)]);
    }

    /// Exactly enough is enough, and one pixel over is not.
    #[test]
    fn the_fit_is_the_whole_test() {
        assert_eq!(packed(&[340, 340], 700, 20), [(0, 2)]);
        assert_eq!(packed(&[340, 341], 700, 20), [(0, 1), (1, 2)]);
    }

    /// A tall category takes a column to itself, and does not stop the ones
    /// after it from pairing up.
    #[test]
    fn a_tall_compartment_keeps_its_own_column() {
        assert_eq!(
            packed(&[700, 200, 200], 700, 20),
            [(0, 1), (1, 3)],
            "the tall one fills its column; the two short ones share the next"
        );
    }

    /// Administrator mode's picker is one compartment holding every
    /// application on the host, and it is taller than any field. It still gets
    /// drawn.
    #[test]
    fn a_compartment_too_tall_for_the_field_is_still_placed() {
        assert_eq!(packed(&[2000], 700, 20), [(0, 1)]);
        assert_eq!(packed(&[2000, 100], 700, 20), [(0, 1), (1, 2)]);
    }

    #[test]
    fn nothing_to_pack_is_no_columns() {
        assert!(pack_into_slots(&[], 700, 20).is_empty());
    }

    /// Config order is never rearranged to make a better fit. A home screen
    /// whose sections move about when one of them gains an activity is worse
    /// than one with a gap in it.
    #[test]
    fn packing_never_reorders_the_categories() {
        // The two short ones would pair happily if the tall one between them
        // could be moved out of the way. It cannot, so all three take a column
        // and the field carries the gap.
        assert_eq!(packed(&[200, 500, 200], 700, 20), [(0, 1), (1, 2), (2, 3)]);

        // The invariant behind that: the columns are contiguous runs covering
        // the categories in order, so reading down a column and then moving
        // right reads the configuration from the top.
        let slots = pack_into_slots(&[300, 300, 700, 100, 100, 100], 700, 20);
        assert_eq!(slots.first().map(|r| r.start), Some(0));
        assert_eq!(slots.last().map(|r| r.end), Some(6));
        for pair in slots.windows(2) {
            assert_eq!(pair[0].end, pair[1].start, "no category is skipped");
        }
    }

    /// The width floor is two item columns and the gap between them — the
    /// same geometry the items themselves are laid out on, so a compartment
    /// holding one stack is exactly as wide as one holding two.
    #[test]
    fn the_narrowest_compartment_is_two_columns_wide() {
        assert_eq!(
            columns_floor(theme::ITEM_W, 1.0),
            2 * theme::ITEM_W + STACK_GAP,
            "a floor that is not a whole number of columns would leave a \
             compartment wider than its items but narrower than the next stack"
        );
        assert!(columns_floor(theme::ITEM_W, 1.0) > theme::ITEM_W);
        // And it comes down with the cells when the row has to squish.
        assert!(columns_floor(theme::ITEM_W - 10, 1.0) < columns_floor(theme::ITEM_W, 1.0));
    }

    #[test]
    fn an_empty_category_has_no_stacks() {
        assert!(split_into_stacks(vec![], THREE).is_empty());
    }

    /// The arithmetic behind `rows_that_fit`: n cells and n-1 gaps.
    #[test]
    fn rows_fill_the_room_they_are_given() {
        // Three 132px cells with 8px between them need 412px, and a fourth
        // needs 552 — so anything from 412 to 551 is a three-row screen.
        assert_eq!(rows_in(412, 132, 8), 3);
        assert_eq!(rows_in(551, 132, 8), 3);
        assert_eq!(rows_in(552, 132, 8), 4, "exactly enough is enough");
        assert_eq!(rows_in(411, 132, 8), 2, "one pixel short is two rows");
    }

    /// A screen with no room for even one item still shows one. Better a
    /// clipped activity than an empty tin: the empty tin says the category is
    /// gone, which is a lie.
    #[test]
    fn there_is_always_at_least_one_row() {
        assert_eq!(rows_in(0, 132, 8), 1);
        assert_eq!(rows_in(-500, 132, 8), 1);
    }

    /// A measurement that came back nonsense falls back to the design's own
    /// number rather than to whatever the division would have produced.
    #[test]
    fn an_unmeasurable_cell_falls_back_to_the_design() {
        assert_eq!(rows_in(600, 0, 8), theme::ROWS_PER_STACK);
    }

    #[test]
    fn earning_wins_over_banking_on_a_category() {
        let mut g = group();
        g.earns_tokens = true;
        g.tokens = Some(lunchbox_api::TokenStatus {
            balance: Duration::from_secs(1500),
            minimum: Duration::ZERO,
            unlocked: true,
            max_balance: None,
            carry_over: false,
        });
        assert_eq!(category_badge(&g), Some(Badge::Earn));
    }

    #[test]
    fn a_gated_category_wears_its_balance() {
        let mut g = group();
        g.tokens = Some(lunchbox_api::TokenStatus {
            balance: Duration::from_secs(25 * 60),
            minimum: Duration::from_secs(10 * 60),
            unlocked: true,
            max_balance: None,
            carry_over: false,
        });
        assert_eq!(category_badge(&g), Some(Badge::Bank { minutes: 25 }));
    }

    #[test]
    fn an_ordinary_category_wears_nothing() {
        assert_eq!(category_badge(&group()), None);
    }

    #[test]
    fn an_item_does_not_repeat_the_earn_pill_its_category_already_wears() {
        let mut e = entry("tuxmath");
        e.earns_tokens = true;
        let now = lunchbox_util::now();
        assert_eq!(
            item_badge(&e, Some(&Badge::Earn), now),
            None,
            "the category says it once; saying it again on every member buries \
             the items that have something of their own to report"
        );
        assert_eq!(item_badge(&e, None, now), Some(Badge::Earn));
    }

    #[test]
    fn an_items_own_gate_shows_even_inside_an_earning_category() {
        let mut e = entry("celeste");
        e.earns_tokens = true;
        e.tokens = Some(lunchbox_api::TokenStatus {
            balance: Duration::from_secs(5 * 60),
            minimum: Duration::from_secs(10 * 60),
            unlocked: false,
            max_balance: None,
            carry_over: false,
        });
        assert_eq!(
            item_badge(&e, Some(&Badge::Earn), lunchbox_util::now()),
            Some(Badge::Need { have: 5, need: 10 }),
            "the gate is this item's own, so the compartment cannot speak for it"
        );
    }

    #[test]
    fn the_nearest_obstacle_is_the_one_shown() {
        // Banked time the child cannot spend for another eight minutes is a
        // misleading thing to put in front of them.
        let now = lunchbox_util::now();
        let mut e = entry("celeste");
        e.reasons = vec![lunchbox_api::ReasonCode::CooldownActive {
            available_at: now + chrono::Duration::seconds(8 * 60),
        }];
        e.tokens = Some(lunchbox_api::TokenStatus {
            balance: Duration::from_secs(25 * 60),
            minimum: Duration::ZERO,
            unlocked: true,
            max_balance: None,
            carry_over: false,
        });
        assert_eq!(item_badge(&e, None, now), Some(Badge::Wait { minutes: 8 }));
    }

    #[test]
    fn an_ordinary_item_wears_nothing() {
        assert_eq!(
            item_badge(&entry("krita"), None, lunchbox_util::now()),
            None
        );
    }

    /// The floor of the compartment, as the child reads it.
    fn floor_of(group: &GroupView) -> Option<String> {
        schedule_line(group).map(|s| s.text())
    }

    fn six_pm() -> chrono::DateTime<chrono::Local> {
        chrono::Local
            .with_ymd_and_hms(2026, 9, 19, 18, 0, 0)
            .unwrap()
    }

    #[test]
    fn an_open_category_floor_says_when_it_shuts() {
        let mut g = group();
        g.window_closes_at = Some(six_pm());
        assert_eq!(floor_of(&g), Some("Until 6:00 PM".into()));
    }

    /// The other half, and the one the brief left to be designed: a category
    /// outside its hours says when it comes back rather than only that it is
    /// gone.
    #[test]
    fn a_shut_category_floor_says_when_it_opens() {
        let mut g = group();
        g.enabled = false;
        g.reasons = vec![ReasonCode::OutsideTimeWindow {
            next_window_start: Some(
                chrono::Local
                    .with_ymd_and_hms(2026, 9, 20, 10, 0, 0)
                    .unwrap(),
            ),
        }];
        assert_eq!(floor_of(&g), Some("Opens 10:00 AM".into()));
    }

    /// Opening wins over closing. A category can carry both — it is outside
    /// today's window and the view still knows when that window ended — and
    /// the useful half is the one that has not happened yet.
    #[test]
    fn the_hour_that_has_not_happened_yet_is_the_one_shown() {
        let mut g = group();
        g.window_closes_at = Some(six_pm());
        g.reasons = vec![ReasonCode::OutsideTimeWindow {
            next_window_start: Some(
                chrono::Local
                    .with_ymd_and_hms(2026, 9, 20, 10, 0, 0)
                    .unwrap(),
            ),
        }];
        assert_eq!(floor_of(&g), Some("Opens 10:00 AM".into()));
    }

    /// A category with no schedule at all has no floor: an empty rule under
    /// an always-available category is a line about nothing.
    #[test]
    fn a_category_with_no_hours_has_no_floor() {
        assert_eq!(floor_of(&group()), None);
    }

    /// And one shut for a reason that is not the clock keeps quiet rather
    /// than inventing an hour. A spent quota comes back at midnight, which is
    /// not something to put in front of a child as a time to wait for.
    #[test]
    fn a_category_shut_for_some_other_reason_has_no_hour_to_give() {
        let mut g = group();
        g.enabled = false;
        g.reasons = vec![ReasonCode::QuotaExhausted {
            used: std::time::Duration::from_secs(3600),
            quota: std::time::Duration::from_secs(3600),
        }];
        assert_eq!(floor_of(&g), None);
    }

    #[test]
    fn the_floor_reads_as_a_wall_clock_time() {
        let at = chrono::Local
            .with_ymd_and_hms(2026, 9, 19, 18, 0, 0)
            .unwrap();
        assert_eq!(format_clock(at), "6:00 PM");
        let morning = chrono::Local
            .with_ymd_and_hms(2026, 9, 19, 9, 5, 0)
            .unwrap();
        assert_eq!(
            format_clock(morning),
            "9:05 AM",
            "no leading zero on the hour"
        );
    }
}
