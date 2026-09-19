//! Availability expanded into per-day minute spans, for the schedule grid.
//!
//! The grid must show what the engine actually does, which is less obvious than
//! it looks. Three behaviours are reproduced here exactly:
//!
//! * **No windows means always available** (`AvailabilityPolicy::is_available`
//!   returns true for an empty window list), as does `always = true`. An
//!   unconfigured activity is on all week, not off.
//! * **`end` is exclusive**, so a window with `start == end` is empty rather
//!   than all-day.
//! * **A cross-midnight window does not cross days.** `TimeWindow::contains`
//!   tests the day mask against the weekday of the *evaluated instant* and only
//!   then wraps the clock, so `days = ["fri"], 22:00-02:00` means Friday
//!   00:00-02:00 and Friday 22:00-24:00 — two spans in one column, not Friday
//!   night running into Saturday.
//!
//! Entry and group availability are checked independently by the engine
//! (`engine.rs` gates on the entry's windows and again on its group's), so the
//! effective result is their intersection.

use serde::Serialize;
use lunchbox_config::{RawAvailability, RawConfig, parse_days, parse_time};

pub const MINUTES_PER_DAY: u16 = 1440;

/// A half-open span of minutes from local midnight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Span {
    pub start: u16,
    pub end: u16,
}

/// Seven days of spans, Monday first, matching the day bitmask's bit order.
pub type Week = Vec<Vec<Span>>;

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AvailabilityView {
    /// The activity's own windows.
    pub entry: Week,
    /// Its group's windows, when it belongs to one.
    pub group: Option<Week>,
    /// What actually happens: the intersection.
    pub effective: Week,
    /// True when the activity places no restriction of its own (no windows, or
    /// `always`), so the UI can say so rather than drawing a full week of bars.
    pub entry_unrestricted: bool,
    /// Same, for the group.
    pub group_unrestricted: bool,
    /// Windows whose `start`/`end`/`days` failed to parse. These are dropped
    /// from the spans above and reported so the grid can flag them.
    pub invalid_windows: Vec<usize>,
}

fn full_week() -> Week {
    (0..7)
        .map(|_| {
            vec![Span {
                start: 0,
                end: MINUTES_PER_DAY,
            }]
        })
        .collect()
}

/// Expand one availability block into per-day spans.
///
/// Returns the week plus the indices of any windows that could not be parsed.
pub fn expand(avail: Option<&RawAvailability>) -> (Week, bool, Vec<usize>) {
    let Some(avail) = avail else {
        return (full_week(), true, Vec::new());
    };
    if avail.always || avail.windows.is_empty() {
        return (full_week(), true, Vec::new());
    }

    let mut week: Week = vec![Vec::new(); 7];
    let mut invalid = Vec::new();

    for (i, w) in avail.windows.iter().enumerate() {
        let (Ok(mask), Ok((sh, sm)), Ok((eh, em))) = (
            parse_days(&w.days),
            parse_time(&w.start),
            parse_time(&w.end),
        ) else {
            invalid.push(i);
            continue;
        };
        let start = sh as u16 * 60 + sm as u16;
        let end = eh as u16 * 60 + em as u16;

        for day in 0..7u8 {
            if mask & (1 << day) == 0 {
                continue;
            }
            let slot = &mut week[day as usize];
            match start.cmp(&end) {
                // `time >= start && time < end`
                std::cmp::Ordering::Less => slot.push(Span { start, end }),
                // Exclusive end makes a zero-width window match nothing.
                std::cmp::Ordering::Equal => {}
                // `time >= start || time < end`, evaluated on this same weekday.
                std::cmp::Ordering::Greater => {
                    slot.push(Span {
                        start,
                        end: MINUTES_PER_DAY,
                    });
                    slot.push(Span { start: 0, end });
                }
            }
        }
    }

    for day in week.iter_mut() {
        *day = merge(std::mem::take(day));
    }
    (week, false, invalid)
}

/// Sort and coalesce overlapping or touching spans.
pub fn merge(mut spans: Vec<Span>) -> Vec<Span> {
    if spans.is_empty() {
        return spans;
    }
    spans.sort_by_key(|s| (s.start, s.end));
    let mut out: Vec<Span> = Vec::with_capacity(spans.len());
    for s in spans {
        match out.last_mut() {
            Some(last) if s.start <= last.end => {
                if s.end > last.end {
                    last.end = s.end;
                }
            }
            _ => out.push(s),
        }
    }
    out
}

/// Overlap of two merged span lists.
pub fn intersect(a: &[Span], b: &[Span]) -> Vec<Span> {
    let mut out = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        let start = a[i].start.max(b[j].start);
        let end = a[i].end.min(b[j].end);
        if start < end {
            out.push(Span { start, end });
        }
        if a[i].end < b[j].end {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

fn intersect_weeks(a: &Week, b: &Week) -> Week {
    a.iter().zip(b).map(|(x, y)| intersect(x, y)).collect()
}

/// Build the grid's view of one activity's availability.
pub fn view_for_entry(config: &RawConfig, entry_id: &str) -> Option<AvailabilityView> {
    let entry = config.entries.iter().find(|e| e.id == entry_id)?;
    let (entry_week, entry_unrestricted, invalid_windows) = expand(entry.availability.as_ref());

    let group = entry
        .group
        .as_ref()
        .and_then(|gid| config.groups.iter().find(|g| &g.id == gid));

    match group {
        Some(g) => {
            let (group_week, group_unrestricted, _) = expand(g.availability.as_ref());
            let effective = intersect_weeks(&entry_week, &group_week);
            Some(AvailabilityView {
                entry: entry_week,
                group: Some(group_week),
                effective,
                entry_unrestricted,
                group_unrestricted,
                invalid_windows,
            })
        }
        None => Some(AvailabilityView {
            effective: entry_week.clone(),
            entry: entry_week,
            group: None,
            entry_unrestricted,
            group_unrestricted: true,
            invalid_windows,
        }),
    }
}

/// The grid's view of a category's own availability.
pub fn view_for_group(config: &RawConfig, group_id: &str) -> Option<AvailabilityView> {
    let group = config.groups.iter().find(|g| g.id == group_id)?;
    let (week, unrestricted, invalid_windows) = expand(group.availability.as_ref());
    Some(AvailabilityView {
        effective: week.clone(),
        entry: week,
        group: None,
        entry_unrestricted: unrestricted,
        group_unrestricted: true,
        invalid_windows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn avail(toml_src: &str) -> RawAvailability {
        toml::from_str(toml_src).expect("availability parses")
    }

    #[test]
    fn no_windows_means_always_available() {
        let (week, unrestricted, _) = expand(Some(&avail("")));
        assert!(unrestricted);
        for day in &week {
            assert_eq!(
                day,
                &[Span {
                    start: 0,
                    end: 1440
                }]
            );
        }
    }

    #[test]
    fn missing_availability_means_always_available() {
        let (week, unrestricted, _) = expand(None);
        assert!(unrestricted);
        assert_eq!(
            week[0],
            [Span {
                start: 0,
                end: 1440
            }]
        );
    }

    #[test]
    fn always_overrides_windows() {
        let (_, unrestricted, _) = expand(Some(&avail(
            r#"
            always = true
            [[windows]]
            days = "weekdays"
            start = "16:00"
            end = "18:00"
            "#,
        )));
        assert!(unrestricted);
    }

    #[test]
    fn weekday_preset_covers_monday_to_friday_only() {
        let (week, _, _) = expand(Some(&avail(
            r#"
            [[windows]]
            days = "weekdays"
            start = "16:00"
            end = "18:00"
            "#,
        )));
        for (d, day) in week.iter().enumerate().take(5) {
            assert_eq!(
                day,
                &[Span {
                    start: 960,
                    end: 1080
                }],
                "day {d}"
            );
        }
        assert!(week[5].is_empty(), "saturday");
        assert!(week[6].is_empty(), "sunday");
    }

    #[test]
    fn cross_midnight_window_stays_on_its_own_weekday() {
        // This is the surprising one: the day mask is tested against the
        // weekday of the instant, so the wrap does not carry into Saturday.
        let (week, _, _) = expand(Some(&avail(
            r#"
            [[windows]]
            days = ["fri"]
            start = "22:00"
            end = "02:00"
            "#,
        )));
        assert_eq!(
            week[4],
            [
                Span { start: 0, end: 120 },
                Span {
                    start: 1320,
                    end: 1440
                }
            ],
            "friday should carry both halves"
        );
        assert!(week[5].is_empty(), "saturday must stay empty");
    }

    #[test]
    fn zero_width_window_matches_nothing() {
        let (week, _, _) = expand(Some(&avail(
            r#"
            [[windows]]
            days = "all"
            start = "09:00"
            end = "09:00"
            "#,
        )));
        assert!(week.iter().all(|d| d.is_empty()));
    }

    #[test]
    fn overlapping_windows_merge() {
        let (week, _, _) = expand(Some(&avail(
            r#"
            [[windows]]
            days = ["mon"]
            start = "09:00"
            end = "11:00"
            [[windows]]
            days = ["mon"]
            start = "10:00"
            end = "12:00"
            "#,
        )));
        assert_eq!(
            week[0],
            [Span {
                start: 540,
                end: 720
            }]
        );
    }

    #[test]
    fn unparseable_windows_are_reported_not_dropped_silently() {
        let (_, _, invalid) = expand(Some(&avail(
            r#"
            [[windows]]
            days = "weekdays"
            start = "nope"
            end = "18:00"
            "#,
        )));
        assert_eq!(invalid, vec![0]);
    }

    #[test]
    fn intersection_is_the_effective_availability() {
        let a = vec![Span {
            start: 540,
            end: 720,
        }];
        let b = vec![Span {
            start: 600,
            end: 900,
        }];
        assert_eq!(
            intersect(&a, &b),
            [Span {
                start: 600,
                end: 720
            }]
        );
    }

    #[test]
    fn group_narrows_the_entry() {
        let config: RawConfig = toml::from_str(
            r#"
            config_version = 1

            [[groups]]
            id = "games"
            label = "Games"
            [groups.availability]
            [[groups.availability.windows]]
            days = "all"
            start = "16:00"
            end = "19:00"

            [[entries]]
            id = "a"
            label = "A"
            group = "games"
            kind = { type = "process", command = "/bin/true" }
            [entries.availability]
            [[entries.availability.windows]]
            days = "all"
            start = "12:00"
            end = "18:00"
            "#,
        )
        .unwrap();

        let v = view_for_entry(&config, "a").unwrap();
        assert_eq!(
            v.effective[0],
            [Span {
                start: 960,
                end: 1080
            }]
        );
        assert!(!v.entry_unrestricted);
        assert!(!v.group_unrestricted);
    }

    #[test]
    fn an_entry_without_a_group_is_its_own_effective_availability() {
        let config: RawConfig = toml::from_str(
            r#"
            config_version = 1
            [[entries]]
            id = "a"
            label = "A"
            kind = { type = "process", command = "/bin/true" }
            "#,
        )
        .unwrap();
        let v = view_for_entry(&config, "a").unwrap();
        assert!(v.entry_unrestricted);
        assert_eq!(
            v.effective[3],
            [Span {
                start: 0,
                end: 1440
            }]
        );
    }
}
