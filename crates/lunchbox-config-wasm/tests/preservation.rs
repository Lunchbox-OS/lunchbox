//! The reason this crate exists: editing must not disturb the rest of the file.
//!
//! `config.example.toml` is 36 KB of which only 10 KB survives a serde
//! round-trip — the rest is comments. These tests hold the line on that.

use lunchbox_config_wasm::{ConfigDoc, Patch};

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

    let before: lunchbox_config::RawConfig = toml::from_str(&src).unwrap();
    let before_ids: Vec<String> = before.entries.iter().map(|e| e.id.clone()).collect();

    doc.apply(
        &Patch::Unset {
            path: "entries[id=tuxmath]".into(),
        },
        None,
    )
    .unwrap();

    let after: lunchbox_config::RawConfig = toml::from_str(&doc.text()).unwrap();
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
            "entries[id=alice-in-wonderland].kind.font_size",
            serde_json::json!(22),
        ),
        None,
    )
    .unwrap();
    doc.apply(
        &set(
            "entries[id=alice-in-wonderland].kind.layout",
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
        .find(|e| e["id"] == "alice-in-wonderland")
        .expect("the example config still has the ebook entry");
    assert_eq!(entry["kind"]["type"], "ebook", "{entry}");
    assert_eq!(entry["kind"]["font_size"], 22, "{entry}");
    assert_eq!(entry["kind"]["layout"], "single", "{entry}");

    let report: serde_json::Value = serde_json::from_str(&doc.validate()).unwrap();
    assert_eq!(report["errors"].as_array().unwrap().len(), 0, "{report}");
}

/// The view spells every unset `Option` as `null`, so a kind the editor read
/// and hands back carries nulls. Written as a standard `[entries.kind]` table
/// it is merged key by key, and a null there has to mean "absent" — as it
/// already did in an inline table — rather than failing the edit (issue #192).
#[test]
fn a_kind_read_from_the_view_can_be_written_back() {
    let mut doc = ConfigDoc::open(&example()).unwrap();
    let view: serde_json::Value = serde_json::from_str(&doc.view().unwrap()).unwrap();
    let mut kind = view["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == "alice-in-wonderland")
        .expect("the example config still has the ebook entry")["kind"]
        .clone();
    assert!(kind["open_at"].is_null(), "sanity: {kind}");

    kind["book"] = serde_json::json!("~/Books/through-the-looking-glass.epub");
    let changed = doc
        .apply(&set("entries[id=alice-in-wonderland].kind", kind), None)
        .unwrap();
    assert!(changed);

    let text = doc.text();
    assert!(
        text.contains(r#"book = "~/Books/through-the-looking-glass.epub""#),
        "got: {text}"
    );
    let hobbit = text
        .split("[[entries]]")
        .find(|s| s.contains(r#"id = "alice-in-wonderland""#))
        .unwrap();
    for key in ["open_at", "command"] {
        assert!(
            !hobbit.lines().any(|l| l.starts_with(key)),
            "{key} was written out: {hobbit}"
        );
    }
    let report: serde_json::Value = serde_json::from_str(&doc.validate()).unwrap();
    assert_eq!(report["errors"].as_array().unwrap().len(), 0, "{report}");
}

/// The same, for every kind in the example, inline or standard table.
#[test]
fn every_kind_in_the_example_can_be_written_back_as_it_reads() {
    let src = example();
    let view: serde_json::Value =
        serde_json::from_str(&ConfigDoc::open(&src).unwrap().view().unwrap()).unwrap();
    for entry in view["entries"].as_array().unwrap() {
        let id = entry["id"].as_str().unwrap();
        let mut doc = ConfigDoc::open(&src).unwrap();
        doc.apply(
            &set(&format!("entries[id={id}].kind"), entry["kind"].clone()),
            None,
        )
        .unwrap_or_else(|e| panic!("{id}: {e}"));
        let report: serde_json::Value = serde_json::from_str(&doc.validate()).unwrap();
        assert_eq!(
            report["errors"].as_array().unwrap().len(),
            0,
            "{id}: {report}"
        );
    }
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

    let raw: lunchbox_config::RawConfig = toml::from_str(&doc.text()).unwrap();
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

// --- reordering ------------------------------------------------------------

fn mv(path: &str, from: usize, to: usize) -> Patch {
    Patch::Move {
        path: path.to_string(),
        from,
        to,
    }
}

fn entry_ids(text: &str) -> Vec<String> {
    let cfg: lunchbox_config::RawConfig = toml::from_str(text).expect("still parses");
    cfg.entries.iter().map(|e| e.id.clone()).collect()
}

/// Moving an entry has to take its sub-tables with it.
///
/// This is the whole risk in reordering `[[entries]]`: an entry is a header
/// plus `[entries.kind]`, `[entries.availability]` and friends, and a move that
/// relocates only the header leaves those behind to be re-read as fields of
/// whichever entry now sits above them. The file still parses and still
/// validates, so the only way to see it is to read the values back.
#[test]
fn a_moved_entry_keeps_its_own_sub_tables() {
    let src = example();
    let before: lunchbox_config::RawConfig = toml::from_str(&src).unwrap();
    let kinds: std::collections::HashMap<String, String> = before
        .entries
        .iter()
        .map(|e| (e.id.clone(), format!("{:?}", e.kind)))
        .collect();

    let mut doc = ConfigDoc::open(&src).unwrap();
    doc.apply(&mv("entries", 0, 2), None).unwrap();
    let text = doc.text();

    let after: lunchbox_config::RawConfig = toml::from_str(&text).expect("still parses");
    for entry in &after.entries {
        assert_eq!(
            format!("{:?}", entry.kind),
            kinds[&entry.id],
            "'{}' came back with another entry's kind",
            entry.id
        );
    }
}

#[test]
fn moving_an_entry_reorders_it_and_nothing_else() {
    let src = example();
    let mut ids = entry_ids(&src);

    let mut doc = ConfigDoc::open(&src).unwrap();
    doc.apply(&mv("entries", 0, 2), None).unwrap();

    let moved = ids.remove(0);
    ids.insert(2, moved);
    let after = doc.text();
    assert_eq!(entry_ids(&after), ids);

    let comments = |t: &str| {
        let mut c: Vec<String> = t
            .lines()
            .filter(|l| l.trim_start().starts_with('#'))
            .map(str::to_string)
            .collect();
        c.sort();
        c
    };
    assert_eq!(
        comments(&after),
        comments(&src),
        "a reorder rearranges comments, it does not lose or invent any"
    );
}

#[test]
fn moving_a_group_reorders_the_categories() {
    let src = example();
    let mut doc = ConfigDoc::open(&src).unwrap();
    doc.apply(&mv("groups", 0, 3), None).unwrap();

    let before: lunchbox_config::RawConfig = toml::from_str(&src).unwrap();
    let after: lunchbox_config::RawConfig = toml::from_str(&doc.text()).expect("still parses");
    let mut expected: Vec<String> = before.groups.iter().map(|g| g.id.clone()).collect();
    let moved = expected.remove(0);
    expected.insert(3, moved);
    assert_eq!(
        after
            .groups
            .iter()
            .map(|g| g.id.clone())
            .collect::<Vec<_>>(),
        expected
    );
}

/// A comment written against an entry travels with it; a section banner does
/// not. `config.example.toml` opens its entries with a banner and a "## ===
/// Native Linux executables ===" heading above the first one, so moving that
/// entry away is exactly the case that would drag the heading down the file.
#[test]
fn a_reorder_leaves_the_section_banner_behind() {
    let src = example();
    let mut doc = ConfigDoc::open(&src).unwrap();
    doc.apply(&mv("entries", 0, 2), None).unwrap();
    let after = doc.text();

    let banner = "## === Native Linux executables ===";
    let entry_comment = "# Tux Math - math games";
    assert!(
        after.find(banner).unwrap() < after.find(entry_comment).unwrap(),
        "the banner stayed at the top of the section"
    );
    assert!(
        after.find(banner).unwrap() < after.find("id = \"scummvm-putt-putt\"").unwrap(),
        "the entry that took first place sits under the banner"
    );
    assert!(
        after.find(entry_comment).unwrap() < after.find("id = \"tuxmath\"").unwrap()
            && after.find("id = \"scummvm-monkey-island\"").unwrap()
                < after.find(entry_comment).unwrap(),
        "Tux Math's own comment came with it"
    );
}

#[test]
fn the_example_config_still_validates_after_a_reorder() {
    let mut doc = ConfigDoc::open(&example()).unwrap();
    doc.apply(&mv("entries", 0, 2), None).unwrap();
    doc.apply(&mv("groups", 4, 0), None).unwrap();

    let report: serde_json::Value = serde_json::from_str(&doc.validate()).unwrap();
    assert_eq!(report["kind"], "semantic", "{report}");
    assert_eq!(report["errors"].as_array().unwrap().len(), 0, "{report}");
}

#[test]
fn a_reorder_is_undone_exactly() {
    let src = example();
    let mut doc = ConfigDoc::open(&src).unwrap();
    doc.apply(&mv("entries", 3, 0), None).unwrap();
    assert!(doc.undo());
    assert_eq!(doc.text(), src);
}

/// Refused rather than guessed at: with another table written in among the
/// entries there is no position to give the one that moves.
#[test]
fn reordering_entries_another_table_is_written_among_is_refused() {
    let mut doc = ConfigDoc::open(
        r#"
config_version = 1

[[entries]]
id = "a"
label = "A"
kind = { type = "process", command = "/bin/true" }

[service.volume]
max_volume = 80

[[entries]]
id = "b"
label = "B"
kind = { type = "process", command = "/bin/true" }
"#,
    )
    .unwrap();
    let before = doc.text();
    assert!(doc.apply(&mv("entries", 0, 1), None).is_err());
    assert_eq!(doc.text(), before, "a refused move changes nothing");
}
