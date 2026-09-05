//! The config editor's default values, rendered from the Rust that decides
//! them instead of mirrored by hand.
//!
//! The editor has to show an *unset* control as the value the daemon will
//! actually pick — an unset switch as on or off, an empty number field with the
//! fallback as its placeholder, a slider with the inherited value on its track.
//! Those answers were spelled out in TypeScript, a `?? true` here and a
//! `const DEFAULT_COOLDOWN_MIN_SESSION = 120` there, which is exactly the drift
//! the generated wire types exist to prevent: `tsc` checks TypeScript against
//! TypeScript and cannot see the Rust rule it claims to copy.
//!
//! Defaults reach here two ways, because the daemon applies them two ways:
//!
//! 1. **Serde defaults** — `#[serde(default = "…")]` on a `RawConfig` field.
//!    `schemars` already writes these into the JSON Schema, so they need no
//!    Rust-side help; this module reads them straight out of
//!    [`config_schema`](crate::config_schema) and groups them by the type they
//!    belong to. Defaults inside `RawEntryKind`'s variants are pulled out
//!    separately, keyed by the `kind.type` an editor is looking at.
//! 2. **Load-time defaults** — `Option<T>` fields whose `None` means "fall
//!    back", resolved in `Policy::from_raw` long after deserialization.
//!    `schemars` sees only `"default": null` for these, so they come from
//!    [`shepherd_config::LoadTimeDefaults`], which exists for this.
//!
//! [`KIND_DEFAULTS`](crate::kind_defaults) is a third thing and stays separate:
//! it answers what a *kind* supplies for a field on the **entry**
//! (`confirm_on_close`, `input_compat`), not what a field inside the kind's own
//! table falls back to.

use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// Render `field-defaults.generated.ts`.
pub fn render() -> String {
    let schema = crate::config_schema::config_schema();
    let root = schema.as_value().as_object().cloned().unwrap_or_default();
    let defs = root
        .get("$defs")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    let mut out = String::new();
    out.push_str(PREAMBLE);
    out.push_str(&render_field_defaults(&defs, &root));
    out.push('\n');
    out.push_str(&render_kind_field_defaults(&defs));
    out.push('\n');
    out.push_str(&render_load_time_defaults());
    out
}

const PREAMBLE: &str = "\
// GENERATED FILE — DO NOT EDIT BY HAND
//
// Rendered from `crates/shepherd-config/src/schema.rs` (serde defaults, via the
// JSON Schema) and `crates/shepherd-config/src/load_defaults.rs` (the ones
// resolved at policy load) by
// `cargo run -p shepherd-wire-codegen --bin rpc-codegen`.
// Edit the Rust and re-run instead.
//
// What a field falls back to when the config leaves it out. The editor needs
// these to render an unset control as the value the daemon will actually pick:
// an unset switch as on or off, an empty number field with its fallback as the
// placeholder, a slider with the inherited value drawn on the track.
//
// Not to be confused with `kind-defaults.generated.ts`, which answers what an
// entry's *kind* supplies for a field on the entry itself.

";

/// The serde defaults of every plain struct, keyed by the type name the editor
/// already imports from `config.generated.ts`.
fn render_field_defaults(defs: &Map<String, Value>, root: &Map<String, Value>) -> String {
    let mut groups: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();

    for (name, def) in defs {
        // Kind variants are rendered separately, keyed by `kind.type` rather
        // than by a type name the editor never names.
        if name == "RawEntryKind" {
            continue;
        }
        let rows = defaults_of(def);
        if !rows.is_empty() {
            groups.insert(name.clone(), rows);
        }
    }
    let root_rows = defaults_of(&Value::Object(root.clone()));
    if !root_rows.is_empty() {
        groups.insert("RawConfig".into(), root_rows);
    }

    let mut out = String::new();
    out.push_str(
        "/**\n * Serde defaults, by the type that declares them.\n *\n \
         * A field absent here has no default: it is either required, or an\n \
         * `Option` the daemon resolves at load time (see `LOAD_TIME_DEFAULTS`).\n */\n\
         export const FIELD_DEFAULTS = {\n",
    );
    for (ty, rows) in &groups {
        out.push_str(&format!("  {ty}: {{\n"));
        for (field, value) in rows {
            out.push_str(&format!("    {field}: {value},\n"));
        }
        out.push_str("  },\n");
    }
    out.push_str("} as const;\n");
    out
}

/// The serde defaults *inside* each `kind` variant, keyed by its `type` tag —
/// `KIND_FIELD_DEFAULTS.retroarch.kiosk`, which is how the editor reaches them.
fn render_kind_field_defaults(defs: &Map<String, Value>) -> String {
    let mut groups: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();

    let variants = defs
        .get("RawEntryKind")
        .and_then(|k| k.get("oneOf"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    for variant in &variants {
        let Some(tag) = variant
            .get("properties")
            .and_then(|p| p.get("type"))
            .and_then(|t| t.get("const"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let mut rows = defaults_of(variant);
        // `type` is the discriminant, not a field anyone leaves unset.
        rows.retain(|(f, _)| f != "type");
        if !rows.is_empty() {
            groups.insert(tag.to_string(), rows);
        }
    }

    let mut out = String::new();
    out.push_str(
        "/**\n * Serde defaults for fields inside an entry's `kind` table,\n \
         * keyed by the `kind.type` they belong to.\n *\n \
         * A kind with nothing to default is absent rather than empty, so a\n \
         * lookup has to cope with `undefined` — `KIND_FIELD_DEFAULTS[k]?.x`.\n */\n\
         export const KIND_FIELD_DEFAULTS = {\n",
    );
    for (tag, rows) in &groups {
        out.push_str(&format!("  {tag}: {{\n"));
        for (field, value) in rows {
            out.push_str(&format!("    {field}: {value},\n"));
        }
        out.push_str("  },\n");
    }
    out.push_str("} as const;\n");
    out
}

/// The defaults `Policy::from_raw` applies, which never reach the schema.
fn render_load_time_defaults() -> String {
    let d = shepherd_config::LoadTimeDefaults::current();
    let json = serde_json::to_value(&d).expect("LoadTimeDefaults serializes");
    let obj = json.as_object().expect("LoadTimeDefaults is a struct");

    let mut out = String::new();
    out.push_str(
        "/**\n * Defaults the daemon resolves at policy load rather than at\n \
         * deserialization, so they are absent from the schema. Units and key\n \
         * names are the config's own.\n */\n\
         export const LOAD_TIME_DEFAULTS = {\n",
    );
    for (field, value) in obj {
        out.push_str(&format!("  {field}: {},\n", ts_literal(value)));
    }
    out.push_str("} as const;\n");
    out
}

/// The `(field, TypeScript literal)` pairs a schema object defaults.
///
/// Two kinds of default are dropped:
///
/// - `"default": null`, which `schemars` emits for an `Option` that merely
///   takes `#[serde(default)]`. It means "absent", not a value to display.
/// - A **populated** object, which is a nested struct expanded in place —
///   `RawConfig.service` comes out as every service key set to null. Those
///   defaults are already rendered under the nested type's own name, so
///   repeating them here would be one more thing to keep in step, and would
///   churn this file on every unrelated schema change. An *empty* object is
///   kept: `env: {}` is a real leaf value.
fn defaults_of(schema: &Value) -> Vec<(String, String)> {
    let Some(props) = schema.get("properties").and_then(Value::as_object) else {
        return Vec::new();
    };
    props
        .iter()
        .filter_map(|(name, field)| {
            let default = field.get("default")?;
            if default.is_null() {
                return None;
            }
            if default.as_object().is_some_and(|o| !o.is_empty()) {
                return None;
            }
            Some((name.clone(), ts_literal(default)))
        })
        .collect()
}

/// A JSON value as a TypeScript literal.
///
/// Objects and arrays are rendered inline rather than via `JSON.stringify`'s
/// spacing so the generated file stays diffable, and object keys are emitted
/// bare — every config key is a valid identifier, which
/// `config_keys_are_bare_identifiers` holds us to.
fn ts_literal(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => format!("{}", serde_json::Value::String(s.clone())),
        Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(ts_literal).collect();
            format!("[{}]", inner.join(", "))
        }
        Value::Object(map) if map.is_empty() => "{}".into(),
        Value::Object(map) => {
            let inner: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{k}: {}", ts_literal(v)))
                .collect();
            format!("{{ {} }}", inner.join(", "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The renderer emits object keys bare, which is only safe while every
    /// config key really is a plain identifier. A key with a dash or a digit
    /// first would produce TypeScript that does not parse.
    #[test]
    fn config_keys_are_bare_identifiers() {
        let rendered = render();
        for line in rendered.lines() {
            let trimmed = line.trim();
            let Some((key, _)) = trimmed.split_once(':') else {
                continue;
            };
            if key.is_empty() || key.starts_with(['*', '/', '}', '{']) || key.contains(' ') {
                continue;
            }
            assert!(
                key.chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                    && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'),
                "key {key:?} is not a bare identifier; ts_literal would emit invalid TypeScript"
            );
        }
    }

    /// Both halves have to actually carry something, or a silent schema change
    /// would leave the editor importing empty objects and falling back to
    /// `undefined` everywhere.
    #[test]
    fn every_section_is_populated() {
        let rendered = render();
        for marker in [
            "FIELD_DEFAULTS",
            "KIND_FIELD_DEFAULTS",
            "LOAD_TIME_DEFAULTS",
        ] {
            let start = rendered.find(marker).expect("section present");
            let body = &rendered[start..];
            let end = body.find("} as const;").expect("section closed");
            assert!(
                body[..end].lines().count() > 3,
                "{marker} rendered empty; the schema or LoadTimeDefaults changed shape"
            );
        }
    }

    /// The values that used to be hand-written in the editor must be the ones
    /// that come out, or the migration silently changes behaviour.
    #[test]
    fn known_defaults_survive_the_round_trip() {
        let rendered = render();
        for expected in [
            "disable_dev_tools: true",           // RawBrowserConfig, serde
            "mode: \"kiosk\"",                   // RawBrowserConfig, serde
            "severity: \"warn\"",                // RawWarningThreshold, serde
            "font_size: 16",                     // ebook variant, serde
            "command: \"retroarch\"",            // retroarch variant, serde
            "cooldown_min_session_seconds: 120", // load time
            "save_grace_seconds: 120",           // load time
            "max_run_seconds: 3600",             // load time
            "management_api_port: 7890",         // load time
        ] {
            assert!(
                rendered.contains(expected),
                "expected {expected:?} in the generated defaults"
            );
        }
    }
}
