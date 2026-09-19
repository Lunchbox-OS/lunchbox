//! The Rust half of the patch wire contract.
//!
//! Every shape in `patch_shapes.json` must deserialize into the variant it
//! claims to be *and* have the effect the editor expects when applied. The
//! TypeScript half lives in `lunchbox-webui/src/config/doc/patches.test.ts` and
//! asserts its builders produce exactly these objects.
//!
//! Without this pairing, a renamed serde tag or field would leave both suites
//! green while the editor silently stopped being able to edit anything.

use lunchbox_config_wasm::{ConfigDoc, Patch};
use serde_json::Value as Json;

const SHAPES: &str = include_str!("patch_shapes.json");

/// A document with everything the fixture's paths address.
const DOC: &str = r#"
config_version = 1

[[entries]]
id = "a"
label = "A"
icon = "a-icon"
kind = { type = "process", command = "/bin/true" }

[entries.availability]
[[entries.availability.windows]]
days = "weekdays"
start = "16:00"
end = "18:00"

[[entries.warnings]]
seconds_before = 60
severity = "warn"

[[entries.warnings]]
seconds_before = 300
severity = "info"
"#;

fn shapes() -> serde_json::Map<String, Json> {
    let parsed: Json = serde_json::from_str(SHAPES).expect("fixture is valid JSON");
    parsed.as_object().expect("fixture is an object").clone()
}

fn shape(name: &str) -> Patch {
    let raw = shapes()
        .get(name)
        .unwrap_or_else(|| panic!("no shape named {name}"))
        .clone();
    serde_json::from_value(raw)
        .unwrap_or_else(|e| panic!("shape {name} does not deserialize into a Patch: {e}"))
}

/// Apply one shape to the sample document and return the result.
fn applied(name: &str) -> String {
    let mut doc = ConfigDoc::open(DOC).expect("sample document parses");
    doc.apply(&shape(name), None)
        .unwrap_or_else(|e| panic!("shape {name} failed to apply: {e}"));
    doc.text()
}

#[test]
fn every_shape_in_the_fixture_deserializes() {
    // Guards against a shape being added to the fixture for the TypeScript side
    // without the Rust side ever seeing it.
    let mut checked = 0;
    for (name, _) in shapes().iter().filter(|(k, _)| !k.starts_with('_')) {
        let _ = shape(name);
        checked += 1;
    }
    assert!(checked >= 13, "expected the full vocabulary, saw {checked}");
}

#[test]
fn set_writes_each_scalar_kind() {
    assert!(applied("set_integer").contains("default_max_run_seconds = 1800"));
    assert!(applied("set_string").contains("label = \"Renamed\""));
    assert!(applied("set_boolean").contains("disabled = true"));
    assert!(applied("set_float").contains("earn_ratio = 0.5"));
}

#[test]
fn set_writes_arrays_and_tables() {
    let arrays = applied("set_array");
    assert!(arrays.contains("keyboard"), "got:\n{arrays}");
    assert!(arrays.contains("mouse"), "got:\n{arrays}");

    let objects = applied("set_object");
    assert!(objects.contains("flatpak"), "got:\n{objects}");
    assert!(objects.contains("org.kde.krita"), "got:\n{objects}");
    assert!(
        !objects.contains("/bin/true"),
        "replacing a table should drop the keys the new value omits:\n{objects}"
    );
}

#[test]
fn set_reaches_through_an_index() {
    let text = applied("set_nested_index");
    assert!(text.contains("start = \"07:30\""), "got:\n{text}");
    assert!(
        text.contains("end = \"18:00\""),
        "the sibling key survives:\n{text}"
    );
}

#[test]
fn unset_removes_keys_elements_and_whole_entries() {
    assert!(!applied("unset_key").contains("a-icon"));

    let raw: lunchbox_config::RawConfig =
        toml::from_str(&applied("unset_array_element")).expect("still parses");
    assert!(
        raw.entries[0]
            .availability
            .as_ref()
            .expect("availability survives")
            .windows
            .is_empty()
    );

    let raw: lunchbox_config::RawConfig =
        toml::from_str(&applied("unset_by_id")).expect("still parses");
    assert!(raw.entries.is_empty(), "the whole entry goes");
}

#[test]
fn insert_appends_and_splices() {
    let raw: lunchbox_config::RawConfig =
        toml::from_str(&applied("insert_append")).expect("still parses");
    let ids: Vec<&str> = raw.entries.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, vec!["a", "added"], "append goes on the end");

    let raw: lunchbox_config::RawConfig =
        toml::from_str(&applied("insert_at_index")).expect("still parses");
    let windows = &raw.entries[0].availability.as_ref().unwrap().windows;
    assert_eq!(windows.len(), 2);
    assert_eq!(windows[0].start, "10:00", "index 0 goes first");
    assert_eq!(windows[1].start, "16:00");
}

#[test]
fn move_reorders_within_an_array() {
    let raw: lunchbox_config::RawConfig =
        toml::from_str(&applied("move_within_array")).expect("still parses");
    let warnings = raw.entries[0].warnings.as_ref().expect("warnings survive");
    assert_eq!(
        warnings
            .iter()
            .map(|w| w.seconds_before)
            .collect::<Vec<_>>(),
        vec![300, 60],
        "the first warning moved to second place"
    );
}

#[test]
fn an_unknown_op_is_rejected_rather_than_ignored() {
    let bogus = serde_json::json!({ "op": "replace", "path": "a", "value": 1 });
    assert!(
        serde_json::from_value::<Patch>(bogus).is_err(),
        "an op this crate does not implement must fail loudly"
    );
}

/// The same op as `insert_at_index`, but landing between two existing tables
/// rather than before both — the case where a stale render position would show
/// up as an element that appears in the wrong place.
#[test]
fn insert_lands_in_the_middle_when_asked() {
    let mut doc = ConfigDoc::open(DOC).expect("sample document parses");
    doc.apply(
        &Patch::Insert {
            path: "entries[id=a].warnings".into(),
            index: Some(1),
            value: serde_json::json!({ "seconds_before": 120, "severity": "warn" }),
        },
        None,
    )
    .expect("insert applies");

    let raw: lunchbox_config::RawConfig = toml::from_str(&doc.text()).expect("still parses");
    let warnings = raw.entries[0].warnings.as_ref().expect("warnings survive");
    assert_eq!(
        warnings
            .iter()
            .map(|w| w.seconds_before)
            .collect::<Vec<_>>(),
        vec![60, 120, 300],
        "the new warning goes between the two that were there"
    );
}
