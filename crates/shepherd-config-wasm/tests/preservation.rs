//! The reason this crate exists: editing must not disturb the rest of the file.
//!
//! `config.example.toml` is 36 KB of which only 10 KB survives a serde
//! round-trip — the rest is comments. These tests hold the line on that.

use shepherd_config_wasm::{ConfigDoc, Patch};

fn example() -> String {
    std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../config.example.toml"
    ))
    .expect("config.example.toml is readable")
}

fn set(path: &str, value: serde_json::Value) -> Patch {
    Patch::Set {
        path: path.to_string(),
        value,
    }
}

/// Lines that differ between two texts, as (before, after) counts.
fn diff_lines(before: &str, after: &str) -> (usize, usize) {
    let b: Vec<&str> = before.lines().collect();
    let a: Vec<&str> = after.lines().collect();
    let common_prefix = b.iter().zip(&a).take_while(|(x, y)| x == y).count();
    let common_suffix = b[common_prefix..]
        .iter()
        .rev()
        .zip(a[common_prefix..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    (
        b.len() - common_prefix - common_suffix,
        a.len() - common_prefix - common_suffix,
    )
}

#[test]
fn a_single_edit_changes_a_single_line() {
    // The phase-0 exit criterion.
    let src = example();
    let mut doc = ConfigDoc::open(&src).unwrap();

    let changed = doc
        .apply(
            &set("service.default_max_run_seconds", serde_json::json!(1800)),
            None,
        )
        .unwrap();
    assert!(changed);

    let after = doc.text();
    assert_eq!(
        diff_lines(&src, &after),
        (1, 1),
        "exactly one line should differ"
    );
    assert!(after.contains("default_max_run_seconds = 1800"));
}

#[test]
fn every_comment_survives_an_edit() {
    let src = example();
    let before_comments = src
        .lines()
        .filter(|l| l.trim_start().starts_with('#'))
        .count();
    assert!(
        before_comments > 100,
        "sanity: the example is comment-heavy"
    );

    let mut doc = ConfigDoc::open(&src).unwrap();
    doc.apply(
        &set("service.default_max_run_seconds", serde_json::json!(60)),
        None,
    )
    .unwrap();
    doc.apply(
        &set("entries[id=tuxmath].label", serde_json::json!("Tux Math!")),
        None,
    )
    .unwrap();

    let after = doc.text();
    let after_comments = after
        .lines()
        .filter(|l| l.trim_start().starts_with('#'))
        .count();
    assert_eq!(before_comments, after_comments);
}

#[test]
fn a_trailing_comment_stays_on_its_value() {
    let src = "config_version = 1\n\n[service]\ndefault_max_run_seconds = 3600  # one hour\n";
    let mut doc = ConfigDoc::open(src).unwrap();
    doc.apply(
        &set("service.default_max_run_seconds", serde_json::json!(1800)),
        None,
    )
    .unwrap();
    assert!(
        doc.text()
            .contains("default_max_run_seconds = 1800  # one hour"),
        "got: {}",
        doc.text()
    );
}

#[test]
fn writing_the_value_it_already_has_does_nothing() {
    let src = example();
    let mut doc = ConfigDoc::open(&src).unwrap();
    let changed = doc
        .apply(
            &set("service.default_max_run_seconds", serde_json::json!(3600)),
            None,
        )
        .unwrap();
    assert!(!changed, "an unchanged value must not touch the document");
    assert_eq!(doc.text(), src);
    assert!(!doc.can_undo(), "and must not cost an undo step");
}

#[test]
fn a_drag_away_and_back_leaves_the_file_byte_identical() {
    let src = example();
    let mut doc = ConfigDoc::open(&src).unwrap();
    for v in [1000, 2000, 3000, 3600] {
        doc.apply(
            &set("service.default_max_run_seconds", serde_json::json!(v)),
            Some("drag:service.default_max_run_seconds"),
        )
        .unwrap();
    }
    assert_eq!(doc.text(), src);
}

#[test]
fn the_example_config_still_validates_after_editing() {
    let src = example();
    let mut doc = ConfigDoc::open(&src).unwrap();
    doc.apply(
        &set(
            "entries[id=tuxmath].limits.max_run_seconds",
            serde_json::json!(900),
        ),
        None,
    )
    .unwrap();

    let report: serde_json::Value = serde_json::from_str(&doc.validate()).unwrap();
    assert_eq!(report["kind"], "semantic", "{report}");
    assert_eq!(report["errors"].as_array().unwrap().len(), 0, "{report}");
}

#[test]
fn editing_one_entry_does_not_touch_its_neighbours() {
    let src = example();
    let mut doc = ConfigDoc::open(&src).unwrap();
    doc.apply(
        &set("entries[id=tuxmath].label", serde_json::json!("Renamed")),
        None,
    )
    .unwrap();
    let after = doc.text();
    assert_eq!(diff_lines(&src, &after), (1, 1));
}

#[test]
fn deleting_an_entry_leaves_the_others_intact() {
    let src = example();
    let mut doc = ConfigDoc::open(&src).unwrap();

    let before: shepherd_config::RawConfig = toml::from_str(&src).unwrap();
    let before_ids: Vec<String> = before.entries.iter().map(|e| e.id.clone()).collect();

    doc.apply(
        &Patch::Unset {
            path: "entries[id=tuxmath]".into(),
        },
        None,
    )
    .unwrap();

    let after: shepherd_config::RawConfig = toml::from_str(&doc.text()).unwrap();
    let after_ids: Vec<String> = after.entries.iter().map(|e| e.id.clone()).collect();

    let expected: Vec<&String> = before_ids.iter().filter(|id| *id != "tuxmath").collect();
    assert_eq!(after_ids.iter().collect::<Vec<_>>(), expected);
}

#[test]
fn a_new_entry_lands_as_an_array_of_tables() {
    let mut doc = ConfigDoc::open("config_version = 1\n").unwrap();
    doc.apply(
        &Patch::Insert {
            path: "entries".into(),
            index: None,
            value: serde_json::json!({
                "id": "new-thing",
                "label": "New Thing",
                "kind": { "type": "process", "command": "/bin/true" },
            }),
        },
        None,
    )
    .unwrap();

    let text = doc.text();
    assert!(text.contains("[[entries]]"), "got: {text}");

    let report: serde_json::Value = serde_json::from_str(&doc.validate()).unwrap();
    assert_eq!(report["kind"], "semantic", "{report}");
    assert_eq!(report["errors"].as_array().unwrap().len(), 0, "{report}");
}

/// A field on the newest kind survives the round trip the editor makes.
///
/// The failure this guards against is silent and specific (see CONTRIBUTING,
/// "Rebuild it after changing the config schema"): the editor's write path is
/// `toml_edit` and needs no schema, so a parser that does not know a field
/// accepts the edit, writes it to the document, and then drops it on the way
/// back — a control that renders, refuses to hold its value, and reports
/// nothing wrong. Reading the value back through `view()` is what catches it.
#[test]
fn an_ebook_entrys_own_fields_survive_the_round_trip() {
    let mut doc = ConfigDoc::open(&example()).unwrap();
    doc.apply(
        &set(
            "entries[id=the-hobbit].kind.font_size",
            serde_json::json!(22),
        ),
        None,
    )
    .unwrap();
    doc.apply(
        &set(
            "entries[id=the-hobbit].kind.layout",
            serde_json::json!("single"),
        ),
        None,
    )
    .unwrap();

    assert!(doc.text().contains("font_size = 22"), "got: {}", doc.text());

    let view: serde_json::Value = serde_json::from_str(&doc.view().unwrap()).unwrap();
    let entry = view["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == "the-hobbit")
        .expect("the example config still has the ebook entry");
    assert_eq!(entry["kind"]["type"], "ebook", "{entry}");
    assert_eq!(entry["kind"]["font_size"], 22, "{entry}");
    assert_eq!(entry["kind"]["layout"], "single", "{entry}");

    let report: serde_json::Value = serde_json::from_str(&doc.validate()).unwrap();
    assert_eq!(report["errors"].as_array().unwrap().len(), 0, "{report}");
}

#[test]
fn creating_a_missing_sub_table_uses_standard_table_style() {
    // `[entries.limits]`, not `limits = { ... }`, matching the house style.
    let src = "config_version = 1\n\n[[entries]]\nid = \"a\"\nlabel = \"A\"\nkind = { type = \"process\", command = \"/bin/true\" }\n";
    let mut doc = ConfigDoc::open(src).unwrap();
    doc.apply(
        &set(
            "entries[id=a].limits.max_run_seconds",
            serde_json::json!(600),
        ),
        None,
    )
    .unwrap();
    let text = doc.text();
    assert!(text.contains("[entries.limits]"), "got: {text}");
    assert!(text.contains("max_run_seconds = 600"), "got: {text}");
}

#[test]
fn undo_restores_the_exact_bytes() {
    let src = example();
    let mut doc = ConfigDoc::open(&src).unwrap();
    doc.apply(
        &set("service.default_max_run_seconds", serde_json::json!(60)),
        None,
    )
    .unwrap();
    assert_ne!(doc.text(), src);
    assert!(doc.undo());
    assert_eq!(
        doc.text(),
        src,
        "undo must be byte-exact, comments included"
    );
}

#[test]
fn a_coalesced_gesture_is_one_undo_step() {
    let mut doc =
        ConfigDoc::open("config_version = 1\n\n[service]\ndefault_max_run_seconds = 60\n").unwrap();
    let key = Some("drag:service.default_max_run_seconds");
    for v in [120, 180, 240, 300] {
        doc.apply(
            &set("service.default_max_run_seconds", serde_json::json!(v)),
            key,
        )
        .unwrap();
    }
    assert!(doc.undo());
    assert!(
        doc.text().contains("default_max_run_seconds = 60"),
        "one undo should return to before the whole drag, got: {}",
        doc.text()
    );
    assert!(!doc.can_undo());
}

#[test]
fn ending_a_gesture_starts_a_new_undo_step() {
    let mut doc =
        ConfigDoc::open("config_version = 1\n\n[service]\ndefault_max_run_seconds = 60\n").unwrap();
    doc.apply(
        &set("service.default_max_run_seconds", serde_json::json!(120)),
        Some("drag"),
    )
    .unwrap();
    doc.end_gesture();
    doc.apply(
        &set("service.default_max_run_seconds", serde_json::json!(180)),
        Some("drag"),
    )
    .unwrap();

    assert!(doc.undo());
    assert!(doc.text().contains("= 120"), "got: {}", doc.text());
    assert!(doc.undo());
    assert!(doc.text().contains("= 60"), "got: {}", doc.text());
}

#[test]
fn redo_replays_what_undo_took_back() {
    let mut doc =
        ConfigDoc::open("config_version = 1\n\n[service]\ndefault_max_run_seconds = 60\n").unwrap();
    doc.apply(
        &set("service.default_max_run_seconds", serde_json::json!(120)),
        None,
    )
    .unwrap();
    doc.undo();
    assert!(doc.redo());
    assert!(doc.text().contains("= 120"));
}

#[test]
fn a_failed_patch_leaves_the_document_untouched() {
    let src = example();
    let mut doc = ConfigDoc::open(&src).unwrap();
    let result = doc.apply(
        &set("entries[id=does-not-exist].label", serde_json::json!("x")),
        None,
    );
    assert!(result.is_err());
    assert_eq!(doc.text(), src);
    assert!(!doc.can_undo());
}

#[test]
fn window_edits_reach_the_right_window() {
    let src = example();
    let mut doc = ConfigDoc::open(&src).unwrap();
    doc.apply(
        &set(
            "entries[id=tuxmath].availability.windows[0].start",
            serde_json::json!("07:30"),
        ),
        None,
    )
    .unwrap();

    let raw: shepherd_config::RawConfig = toml::from_str(&doc.text()).unwrap();
    let entry = raw.entries.iter().find(|e| e.id == "tuxmath").unwrap();
    assert_eq!(
        entry.availability.as_ref().unwrap().windows[0].start,
        "07:30"
    );
    assert_eq!(diff_lines(&src, &doc.text()), (1, 1));
}

#[test]
fn setting_a_whole_table_replaces_only_the_keys_it_names() {
    let src = "config_version = 1\n\n[[entries]]\nid = \"a\"\nlabel = \"A\"\nkind = { type = \"process\", command = \"/bin/true\" }\n\n[entries.limits]\n# how long a sitting may last\nmax_run_seconds = 600\ndaily_quota_seconds = 3600\n";
    let mut doc = ConfigDoc::open(src).unwrap();
    doc.apply(
        &set(
            "entries[id=a].limits",
            serde_json::json!({ "max_run_seconds": 900, "daily_quota_seconds": 3600 }),
        ),
        None,
    )
    .unwrap();
    let text = doc.text();
    assert!(
        text.contains("# how long a sitting may last"),
        "got: {text}"
    );
    assert!(text.contains("max_run_seconds = 900"), "got: {text}");
}
