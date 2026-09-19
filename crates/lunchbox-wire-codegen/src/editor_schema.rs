//! JSON Schema for the types crossing the config editor's wasm boundary.
//!
//! `lunchbox-config-wasm` hands the editor three JSON payloads: the `RawConfig`
//! projection, a validation [`Report`], and an [`AvailabilityView`] per
//! subject. The first has been generated from `schema.rs` since the editor
//! shipped; the other two were mirrored by hand in
//! `lunchbox-webui/src/config/model/`, each with a header naming the Rust file
//! it copied — a promise nothing enforced.
//!
//! The drift path was asymmetric in the way that matters. Add a
//! `ValidationError` variant and the `From<&ValidationError> for Issue` match
//! in `report.rs` is exhaustive, so the compiler makes you handle it; nothing
//! made you touch the TypeScript. The Rust stayed correct by construction while
//! the mirror went stale — the same shape as the two Kotlin drifts that
//! motivated [`crate::wire_schema`].
//!
//! Generating these needed `Issue::kind` to stop being a `&'static str` first:
//! `schemars` renders that as a bare `string`, which would have *lost* the
//! eight-name union the hand-written file had. It is [`IssueKind`] now, so the
//! generated union is the real one.

use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde_json::{Map, Value};

/// Every type the editor decodes out of the wasm module.
///
/// One struct so `schema_for!` walks the graph from a single root and lands
/// everything reachable in one `$defs` block, exactly as
/// [`crate::wire_schema`] does.
#[derive(JsonSchema)]
#[allow(dead_code)]
struct EditorTypes {
    report: lunchbox_config_wasm::report::Report,
    issue: lunchbox_config_wasm::report::Issue,
    issue_kind: lunchbox_config_wasm::report::IssueKind,
    availability_view: lunchbox_config_wasm::windows::AvailabilityView,
    span: lunchbox_config_wasm::windows::Span,
    versions: lunchbox_config_wasm::Versions,
}

/// The `$defs` block describing each of those, keyed by Rust type name.
pub fn editor_schema() -> Map<String, Value> {
    let mut generator = SchemaGenerator::default();
    let schema: Schema = generator.root_schema_for::<EditorTypes>();

    schema
        .as_value()
        .get("$defs")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_type_the_editor_decodes_is_described() {
        let defs = editor_schema();
        for name in [
            "Report",
            "Issue",
            "IssueKind",
            "AvailabilityView",
            "Span",
            "Versions",
        ] {
            assert!(defs.contains_key(name), "missing {name} from $defs");
        }
    }

    #[test]
    fn issue_kind_records_every_validation_error() {
        // The whole point of making `kind` an enum: had it stayed a
        // `&'static str`, this schema would say `"type": "string"` and the
        // generated union would be gone.
        let defs = editor_schema();
        let rendered = serde_json::to_string(defs.get("IssueKind").expect("IssueKind")).unwrap();
        for kind in [
            "entry",
            "group",
            "duplicate_entry_id",
            "duplicate_group_id",
            "invalid_time_format",
            "invalid_day_spec",
            "warning_exceeds_max_run",
            "global",
        ] {
            assert!(rendered.contains(kind), "IssueKind schema lacks {kind}");
        }
    }

    #[test]
    fn a_report_keeps_its_three_kinds_apart() {
        // The editor reacts to each differently, so they must stay a tagged
        // union rather than collapsing into one optional-everything object.
        let defs = editor_schema();
        let rendered = serde_json::to_string(defs.get("Report").expect("Report")).unwrap();
        for kind in ["syntax", "version", "semantic"] {
            assert!(rendered.contains(kind), "Report schema lacks {kind}");
        }
    }
}
