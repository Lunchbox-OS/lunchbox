//! One category: a cream compartment sunk into the enamel field.
//!
//! A compartment is a *well*, not a card. That is the whole of its visual
//! character and the reason it carries inset shadows and no drop shadow — the
//! CSS in `theme.rs` does the drawing; this file decides what goes in it.
//!
//! Its contents, top to bottom: the category's name and (at most) one badge,
//! then the items in stacks of three that spill to the right, then — only when
//! the category actually shuts today — a hairline floor and the closing time.

use gtk4::prelude::*;
use lunchbox_api::{EntryView, GroupView};

use crate::badge::Badge;
use crate::item::LauncherItem;
use crate::theme;

/// A category on screen, and the items it holds in the order they are drawn.
pub struct Compartment {
    /// The well itself, to put in the row.
    pub widget: gtk4::Widget,
    /// The items, grouped into the stacks they were laid out in. The field's
    /// D-pad model is built on exactly this shape: left/right move between
    /// stacks, up/down within one.
    pub stacks: Vec<Vec<LauncherItem>>,
}

/// Build the compartment for a category.
///
/// `group` is `None` for the implicit trailing category that collects entries
/// belonging to no group; it has a name but no schedule and no shared gate, so
/// it never carries a badge or a floor.
pub fn build(
    label: &str,
    group: Option<&GroupView>,
    entries: Vec<EntryView>,
    scale: f64,
) -> Compartment {
    let well = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    well.add_css_class("lb-compartment");
    // Every compartment is the full height of the field, so the row reads as
    // one tin rather than a skyline. Width is what grows when a category has
    // more than three members (§3 of the brief).
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

    // ------------------------------------------------------------- header
    let header = gtk4::Box::new(gtk4::Orientation::Horizontal, theme::px(10, scale));
    header.add_css_class("lb-compartment__header");

    let name = gtk4::Label::new(Some(label));
    name.add_css_class("lb-compartment__name");
    name.set_xalign(0.0);
    // The name takes the slack, which pushes the badge to the far end of the
    // header instead of leaving it tucked against the name.
    //
    // A deliberate departure from the mockup, which sets the badge immediately
    // after the category name. Across a row of compartments of different widths
    // that puts every badge at a different offset; at the end they line up with
    // each compartment's right edge, and the eye can run down them.
    name.set_hexpand(true);
    name.set_halign(gtk4::Align::Start);
    header.append(&name);

    let category = group.and_then(category_badge);
    if let Some(badge) = &category {
        let badge = badge.widget(scale);
        badge.set_halign(gtk4::Align::End);
        header.append(&badge);
    }
    well.append(&header);

    // -------------------------------------------------------------- items
    let stacks = split_into_stacks(entries);
    let columns = gtk4::Box::new(gtk4::Orientation::Horizontal, theme::px(16, scale));
    columns.set_vexpand(true);
    columns.set_valign(gtk4::Align::Start);

    let mut built: Vec<Vec<LauncherItem>> = Vec::new();
    let now = lunchbox_util::now();
    for stack in stacks {
        let column = gtk4::Box::new(gtk4::Orientation::Vertical, theme::px(8, scale));
        column.set_valign(gtk4::Align::Start);
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
    if let Some(text) = group.and_then(schedule_line) {
        let floor = gtk4::Box::new(gtk4::Orientation::Horizontal, theme::px(8, scale));
        floor.add_css_class("lb-compartment__floor");
        floor.set_valign(gtk4::Align::End);
        floor.set_vexpand(true);

        let schedule = gtk4::Label::new(Some(&text));
        schedule.add_css_class("lb-compartment__schedule");
        schedule.set_xalign(0.0);
        floor.append(&schedule);
        well.append(&floor);
    }

    Compartment {
        widget: well.upcast(),
        stacks: built,
    }
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

/// What the compartment floor says about this category's hours today.
///
/// Only the closing time of the window the category is *currently inside*. A
/// category with no schedule has no floor, and — for now — neither does one
/// that is shut.
///
/// The shut case is the bedtime screen's, which the branding brief leaves to be
/// designed rather than guessed at (§9), and it is blocked on the engine
/// besides: `ReasonCode::OutsideTimeWindow` carries a `next_window_start` that
/// nothing has ever filled in (see the TODO in `Engine::evaluate_entry`). So
/// "Opens 10:00 AM" needs that computed first, and this is the one line that
/// will want it.
fn schedule_line(group: &GroupView) -> Option<String> {
    group
        .window_closes_at
        .map(|closes| format!("Until {}", format_clock(closes)))
}

/// Break a category's members into stacks of `ROWS_PER_STACK`, in config
/// order. Width grows; height never does.
fn split_into_stacks(entries: Vec<EntryView>) -> Vec<Vec<EntryView>> {
    if entries.is_empty() {
        return Vec::new();
    }
    entries
        .chunks(theme::ROWS_PER_STACK)
        .map(<[EntryView]>::to_vec)
        .collect()
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

    #[test]
    fn three_members_are_one_stack() {
        let stacks = split_into_stacks(vec![entry("a"), entry("b"), entry("c")]);
        assert_eq!(stacks.len(), 1);
        assert_eq!(stacks[0].len(), 3);
    }

    #[test]
    fn a_fourth_member_spills_into_a_second_stack_beside_it() {
        // The acceptance checklist: 4-6 members render double-wide, 7-9 triple.
        let four = split_into_stacks((0..4).map(|i| entry(&i.to_string())).collect());
        assert_eq!(four.len(), 2, "4 members are double-wide");
        assert_eq!(four[0].len(), 3);
        assert_eq!(four[1].len(), 1);

        let seven = split_into_stacks((0..7).map(|i| entry(&i.to_string())).collect());
        assert_eq!(seven.len(), 3, "7 members are triple-wide");
    }

    #[test]
    fn config_order_survives_the_split() {
        let stacks = split_into_stacks(vec![
            entry("first"),
            entry("second"),
            entry("third"),
            entry("fourth"),
        ]);
        let flat: Vec<_> = stacks
            .iter()
            .flatten()
            .map(|e| e.entry_id.as_str().to_string())
            .collect();
        assert_eq!(flat, ["first", "second", "third", "fourth"]);
    }

    #[test]
    fn an_empty_category_has_no_stacks() {
        assert!(split_into_stacks(vec![]).is_empty());
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

    #[test]
    fn an_open_category_floor_says_when_it_shuts() {
        let mut g = group();
        g.window_closes_at = Some(
            chrono::Local
                .with_ymd_and_hms(2026, 9, 19, 18, 0, 0)
                .unwrap(),
        );
        assert_eq!(schedule_line(&g), Some("Until 6:00 PM".to_string()));
    }

    #[test]
    fn a_category_with_no_schedule_has_no_floor() {
        assert_eq!(schedule_line(&group()), None);
    }

    #[test]
    fn a_shut_category_has_no_floor_yet() {
        // Deliberate, and the one thing here that is waiting on somebody else:
        // a category outside its hours has no closing time to print, and the
        // "Opens 10:00 AM" that belongs there needs an engine that computes
        // `next_window_start` -- which nothing does yet.
        let mut g = group();
        g.enabled = false;
        g.reasons = vec![lunchbox_api::ReasonCode::OutsideTimeWindow {
            next_window_start: None,
        }];
        assert_eq!(schedule_line(&g), None);
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
