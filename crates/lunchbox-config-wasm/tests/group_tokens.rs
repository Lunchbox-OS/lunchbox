//! Token gates on categories, and the self-unlock rules the editor's source
//! picker encodes.
//!
//! The picker's exclusions live in
//! `shepherd-webui/src/config/model/tokenSources.ts` and are tested there. These
//! are the other half of that pairing: each one asserts the validator really
//! does reject what the picker refuses to offer, so the two cannot drift apart
//! into a picker that hides valid choices or offers invalid ones.

use lunchbox_config_wasm::{ConfigDoc, Patch};

fn set(path: &str, value: serde_json::Value) -> Patch {
    Patch::Set {
        path: path.into(),
        value,
    }
}

#[test]
fn a_group_token_gate_can_be_written_and_validates() {
    let src = r#"
config_version = 1

[[groups]]
id = "games"
label = "Games"

[[entries]]
id = "tuxmath"
label = "Tux Math"
kind = { type = "process", command = "/bin/true" }

[[entries]]
id = "celeste"
label = "Celeste"
group = "games"
kind = { type = "process", command = "/bin/true" }
"#;
    let mut doc = ConfigDoc::open(src).unwrap();

    doc.apply(
        &set(
            "groups[id=games].tokens.from",
            serde_json::json!(["tuxmath"]),
        ),
        None,
    )
    .unwrap();
    doc.apply(
        &set("groups[id=games].tokens.earn_ratio", serde_json::json!(0.5)),
        None,
    )
    .unwrap();
    doc.apply(
        &set(
            "groups[id=games].tokens.minimum_seconds",
            serde_json::json!(600),
        ),
        None,
    )
    .unwrap();

    let text = doc.text();
    assert!(text.contains("[groups.tokens]"), "got:\n{text}");

    let report: serde_json::Value = serde_json::from_str(&doc.validate()).unwrap();
    assert_eq!(report["kind"], "semantic", "{report}");
    assert_eq!(report["errors"].as_array().unwrap().len(), 0, "{report}");

    // And the daemon's own loader accepts it.
    let policy = lunchbox_config::parse_config(&text).unwrap();
    assert!(
        policy.groups[0].tokens.is_some(),
        "group gate reached the policy layer"
    );
}

#[test]
fn a_group_gate_listing_its_own_member_is_rejected() {
    // The rule the picker encodes by not offering members.
    let src = r#"
config_version = 1
[[groups]]
id = "games"
label = "Games"
[groups.tokens]
from = ["celeste"]

[[entries]]
id = "celeste"
label = "Celeste"
group = "games"
kind = { type = "process", command = "/bin/true" }
"#;
    let doc = ConfigDoc::open(src).unwrap();
    let report: serde_json::Value = serde_json::from_str(&doc.validate()).unwrap();
    let errors = report["errors"].as_array().unwrap();
    assert_eq!(errors.len(), 1, "{report}");
    assert_eq!(errors[0]["group_id"], "games");
    assert!(
        errors[0]["message"]
            .as_str()
            .unwrap()
            .contains("member of this group"),
        "{report}"
    );
}

/// A config with two categories, members in each, and one ungrouped activity —
/// the same shape the TypeScript test uses.
fn config_with(gate_owner: &str, from: &str) -> String {
    let (entry_gate, group_gate) = match gate_owner {
        "entry" => (
            format!("[entries.tokens]\nfrom = [\"{from}\"]\n"),
            String::new(),
        ),
        "group" => (
            String::new(),
            format!("[groups.tokens]\nfrom = [\"{from}\"]\n"),
        ),
        other => panic!("unknown gate owner {other}"),
    };
    format!(
        r#"
config_version = 1

[[groups]]
id = "games"
label = "Games"
{group_gate}
[[entries]]
id = "celeste"
label = "Celeste"
group = "games"
kind = {{ type = "process", command = "/bin/true" }}
{entry_gate}
[[entries]]
id = "loose"
label = "Loose"
kind = {{ type = "process", command = "/bin/true" }}
"#
    )
}

/// The messages the validator produces for a rejected gate.
fn rejections(toml_src: &str) -> Vec<String> {
    let doc = ConfigDoc::open(toml_src).expect("config parses");
    let report: serde_json::Value = serde_json::from_str(&doc.validate()).unwrap();
    assert_eq!(report["kind"], "semantic", "{report}");
    report["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["message"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn an_activity_cannot_list_itself() {
    let errors = rejections(&config_with("entry", "celeste"));
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].contains("cannot list the entry itself"),
        "{errors:?}"
    );
}

#[test]
fn an_activity_cannot_list_the_category_it_belongs_to() {
    let errors = rejections(&config_with("entry", "group:games"));
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].contains("which this entry belongs to"),
        "{errors:?}"
    );
}

#[test]
fn a_category_cannot_list_itself() {
    let errors = rejections(&config_with("group", "group:games"));
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].contains("cannot list the group itself"),
        "{errors:?}"
    );
}

#[test]
fn what_the_picker_does_offer_is_accepted() {
    // The complement of the four exclusions: an ungrouped activity is a valid
    // source for both a category gate and an activity in another category.
    assert!(rejections(&config_with("group", "loose")).is_empty());
    assert!(rejections(&config_with("entry", "loose")).is_empty());
}
