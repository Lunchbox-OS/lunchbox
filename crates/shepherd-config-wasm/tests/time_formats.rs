//! The daemon's half of the time and day format contract.
//!
//! Every case in `time_formats.json` must parse the way the fixture says. The
//! editor's half is `shepherd-webui/src/config/model/windows.test.ts`, which
//! asserts its TypeScript re-implementation against the same file.
//!
//! Without the pairing the two drift silently, and had: `"16:5"` loaded on the
//! daemon as 16:05 while the grid drew it as unparseable, and `" 16:30"` drew a
//! band for a value the daemon rejects. Neither suite could see it, because
//! each only ever tested its own side.
//!
//! The functions under test live in `shepherd-config`, not here. The fixture
//! sits beside `patch_shapes.json` because this is the directory the editor's
//! tests already reach into for cross-language fixtures.

use serde::Deserialize;
use shepherd_config::{RawDays, parse_days, parse_time};

const FIXTURE: &str = include_str!("time_formats.json");

#[derive(Deserialize)]
struct Fixture {
    times: Vec<TimeCase>,
    day_presets: Vec<PresetCase>,
    day_lists: Vec<ListCase>,
}

#[derive(Deserialize)]
struct TimeCase {
    input: String,
    /// Minutes from midnight, or `null` when the daemon rejects the value.
    minutes: Option<u16>,
}

#[derive(Deserialize)]
struct PresetCase {
    input: String,
    mask: Option<u8>,
}

#[derive(Deserialize)]
struct ListCase {
    input: Vec<String>,
    mask: Option<u8>,
}

fn fixture() -> Fixture {
    serde_json::from_str(FIXTURE).expect("time_formats.json parses")
}

#[test]
fn times_parse_as_the_fixture_says() {
    for case in fixture().times {
        let got = parse_time(&case.input)
            .ok()
            .map(|(h, m)| h as u16 * 60 + m as u16);
        assert_eq!(
            got, case.minutes,
            "parse_time({:?}) — the editor renders this as {:?}",
            case.input, case.minutes
        );
    }
}

#[test]
fn day_presets_parse_as_the_fixture_says() {
    for case in fixture().day_presets {
        let got = parse_days(&RawDays::Preset(case.input.clone())).ok();
        assert_eq!(got, case.mask, "parse_days({:?})", case.input);
    }
}

#[test]
fn day_lists_parse_as_the_fixture_says() {
    for case in fixture().day_lists {
        let got = parse_days(&RawDays::List(case.input.clone())).ok();
        assert_eq!(got, case.mask, "parse_days({:?})", case.input);
    }
}

/// The fixture is only worth having if it covers both outcomes on both sides.
/// A file that drifted into all-accept or all-reject would still pass every
/// assertion above.
#[test]
fn the_fixture_covers_acceptance_and_rejection() {
    let f = fixture();
    let split = |accepted: usize, rejected: usize, what: &str| {
        assert!(accepted > 0, "{what}: no accepted cases");
        assert!(rejected > 0, "{what}: no rejected cases");
    };
    split(
        f.times.iter().filter(|c| c.minutes.is_some()).count(),
        f.times.iter().filter(|c| c.minutes.is_none()).count(),
        "times",
    );
    split(
        f.day_presets.iter().filter(|c| c.mask.is_some()).count(),
        f.day_presets.iter().filter(|c| c.mask.is_none()).count(),
        "day_presets",
    );
    split(
        f.day_lists.iter().filter(|c| c.mask.is_some()).count(),
        f.day_lists.iter().filter(|c| c.mask.is_none()).count(),
        "day_lists",
    );
}
