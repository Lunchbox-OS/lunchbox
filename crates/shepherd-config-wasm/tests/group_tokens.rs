use shepherd_config_wasm::{ConfigDoc, Patch};

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
    let policy = shepherd_config::parse_config(&text).unwrap();
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
