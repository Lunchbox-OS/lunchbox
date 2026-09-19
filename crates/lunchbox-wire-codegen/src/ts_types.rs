//! Render a JSON Schema as TypeScript.
//!
//! Used for both generated TypeScript mirrors: the wire types the web UI
//! decodes, and the `config.toml` types the config editor renders forms from.
//! Those started as separate renderers written weeks apart, which converged on
//! the same shapes — rendering the config schema through this one came out
//! byte-identical to its own output except for declaration order, so the second
//! renderer was deleted rather than left to drift against this one.
//!
//! The two schemas are written differently: the wire schema is hand-rolled in
//! [`crate::wire_schema`], while the config schema is whatever `schemars`
//! derives from `RawConfig`. In practice that costs one branch — `schemars`
//! emits `anyOf` for `#[serde(untagged)]`, which nothing on the wire uses.
//!
//! The counterpart to [`crate::kotlin_types`], and it exists for the same
//! reason: `shepherd-webui/src/api/types.ts` was hand-written, so nothing
//! connected it to the Rust types it mirrors. The drift test compares codegen
//! outputs against their checked-in copies, and a file that is not an output is
//! invisible to it; `tsc` only checks TypeScript against itself and has no idea
//! what shape the daemon sends. That is precisely the gap that let the Kotlin
//! mirrors drift twice (see [`crate::wire_schema`]).
//!
//! # Two deliberate differences from the Kotlin renderer
//!
//! **Everything generates.** Kotlin skips `Event`, `EventPayload` and
//! `LaunchOutcome` because kotlinx cannot express a `$ref` flattened beside its
//! tag, or an externally tagged enum, as a sealed class. TypeScript's types are
//! structural, so both are ordinary: the first is an intersection
//! (`{ type: "state_changed" } & ServiceStateSnapshot`) and the second a union
//! of single-key objects (`{ Approved: … } | { Denied: … }`).
//!
//! **No `Unknown` fallback variant.** The Kotlin sealed interfaces each carry
//! one, because the companion is installed separately and can be older than the
//! device it talks to — that skew is exactly how four missing `ReasonCode`
//! variants broke its entry list. The web UI cannot skew: it is embedded into
//! `lunchboxd` at compile time (`lunchbox-http`'s `web_assets.rs`), so the SPA
//! and the daemon serving it are always the same build. Adding `| (string & {})`
//! to absorb unknown values would buy nothing and would cost exhaustiveness
//! checking on every `switch`, which is the main thing TypeScript offers here.

use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// Types this renderer deliberately skips. Empty, and that is the point: if a
/// type ever cannot be expressed, add it here with the reason rather than
/// emitting something that type-checks and decodes wrongly.
pub const HAND_WRITTEN: &[&str] = &[];

/// Whether a `type` field (string or array) admits null.
fn admits_null(ty: Option<&Value>) -> bool {
    match ty {
        Some(Value::Array(items)) => items.iter().any(|v| v.as_str() == Some("null")),
        _ => false,
    }
}

/// The non-null primitive named by a `type` field.
fn primitive(ty: Option<&Value>) -> Option<String> {
    match ty {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .find(|s| *s != "null")
            .map(str::to_string),
        _ => None,
    }
}

/// Parenthesise a union before suffixing it with `[]`, so `A | B` becomes
/// `(A | B)[]` rather than `A | B[]`, which means something else entirely.
fn as_array_element(ty: &str) -> String {
    if ty.contains('|') || ty.contains("=>") {
        format!("({ty})")
    } else {
        ty.to_string()
    }
}

/// Map a property schema onto a TypeScript type expression.
///
/// `indent` is the current indentation, so an inline object nested inside a
/// property renders its own braces at the right depth.
fn ts_type(schema: &Value, indent: &str) -> String {
    // A reference to another named type.
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        return reference
            .rsplit('/')
            .next()
            .unwrap_or(reference)
            .to_string();
    }

    // `Option<T>` where T is a named type renders as anyOf[$ref, null].
    if let Some(any_of) = schema.get("anyOf").and_then(Value::as_array) {
        let nullable = any_of
            .iter()
            .any(|v| v.get("type").and_then(Value::as_str) == Some("null"));
        let mut parts: Vec<String> = any_of
            .iter()
            .filter(|v| v.get("type").and_then(Value::as_str) != Some("null"))
            .map(|v| ts_type(v, indent))
            .collect();
        if nullable {
            parts.push("null".to_string());
        }
        if !parts.is_empty() {
            return parts.join(" | ");
        }
    }

    // A fieldless enum used inline rather than as a named type.
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        let union = values
            .iter()
            .map(|v| match v.as_str() {
                Some(s) => format!("\"{s}\""),
                None => "unknown".to_string(),
            })
            .collect::<Vec<_>>()
            .join(" | ");
        if !union.is_empty() {
            return union;
        }
    }

    let ty = schema.get("type");
    let nullable = admits_null(ty);
    let base = match primitive(ty).as_deref() {
        Some("string") => match schema.get("format").and_then(Value::as_str) {
            // Aliases of `string`, so callers keep using plain strings while the
            // intent stays readable — the same trick the Kotlin side plays.
            Some("date-time") => "IsoTimestamp".to_string(),
            Some("date") => "IsoDate".to_string(),
            _ => "string".to_string(),
        },
        Some("integer") | Some("number") => "number".to_string(),
        Some("boolean") => "boolean".to_string(),
        Some("array") => {
            let inner = schema
                .get("items")
                .map(|i| ts_type(i, indent))
                .unwrap_or_else(|| "unknown".to_string());
            format!("{}[]", as_array_element(&inner))
        }
        Some("object") => match schema.get("additionalProperties") {
            Some(v) if v.is_object() => format!("Record<string, {}>", ts_type(v, indent)),
            _ if schema.get("properties").is_some() => render_inline_object(schema, indent),
            // An object with neither named properties nor a value type is a
            // free-form map, not `any`: callers should have to narrow it.
            _ => "Record<string, unknown>".to_string(),
        },
        // An untyped node is `serde_json::Value` or similar. `unknown` rather
        // than `any` so it cannot silently spread into everything it touches.
        _ => "unknown".to_string(),
    };

    if nullable {
        format!("{base} | null")
    } else {
        base
    }
}

/// A JSDoc block for a schema's `description`, or nothing.
fn doc_comment(schema: &Value, indent: &str) -> String {
    let Some(desc) = schema.get("description").and_then(Value::as_str) else {
        return String::new();
    };
    let body: Vec<String> = desc
        .lines()
        .map(|l| format!("{indent} * {l}").trim_end().to_string())
        .collect();
    format!("{indent}/**\n{}\n{indent} */\n", body.join("\n"))
}

/// A property name that is not a plain identifier has to be quoted.
fn property_key(name: &str) -> String {
    let plain = !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$');
    if plain {
        name.to_string()
    } else {
        format!("\"{name}\"")
    }
}

/// The property lines of an object schema, each already indented.
fn render_properties(schema: &Value, indent: &str) -> Vec<String> {
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();

    let empty = Map::new();
    let props = schema
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or(&empty);

    props
        .iter()
        .map(|(name, prop)| {
            let ty = ts_type(prop, indent);
            // Absent and null are both possible for a non-required property:
            // serde omits some and writes `null` for others, and a client that
            // only tolerates one of the two breaks on the other.
            let optional = !required.contains(&name.as_str());
            let marker = if optional { "?" } else { "" };
            let doc = doc_comment(prop, indent);
            format!("{doc}{indent}{}{marker}: {ty};", property_key(name))
        })
        .collect()
}

/// An object schema rendered inline, for a property whose type has no name.
fn render_inline_object(schema: &Value, indent: &str) -> String {
    let inner = format!("{indent}  ");
    let lines = render_properties(schema, &inner);
    if lines.is_empty() {
        return "Record<string, never>".to_string();
    }
    format!("{{\n{}\n{indent}}}", lines.join("\n"))
}

/// The tag property of an internally-tagged enum, if this `oneOf` is one.
fn discriminator(variants: &[Value]) -> Option<String> {
    let first = variants.first()?;
    let props = first.get("properties")?.as_object()?;
    let tag = props
        .iter()
        .find(|(_, v)| v.get("const").is_some())
        .map(|(k, _)| k.clone())?;
    variants
        .iter()
        .all(|v| {
            v.get("properties")
                .and_then(Value::as_object)
                .and_then(|p| p.get(&tag))
                .and_then(|t| t.get("const"))
                .is_some()
        })
        .then_some(tag)
}

/// Whether every variant is `{"OneKey": {…}}` — serde's externally tagged form.
fn is_externally_tagged(variants: &[Value]) -> bool {
    !variants.is_empty()
        && variants.iter().all(|v| {
            let Some(props) = v.get("properties").and_then(Value::as_object) else {
                return false;
            };
            props.len() == 1 && props.values().all(|p| p.get("const").is_none())
        })
}

fn render_string_union(name: &str, values: Vec<String>, schema: &Value) -> String {
    let mut out = doc_comment(schema, "");
    out.push_str(&format!("export type {name} =\n"));
    let last = values.len().saturating_sub(1);
    for (i, v) in values.iter().enumerate() {
        let sep = if i == last { ";" } else { "" };
        out.push_str(&format!("  | \"{v}\"{sep}\n"));
    }
    out
}

/// A `oneOf` of string consts, each of which may carry its own docs.
fn render_documented_string_union(name: &str, variants: &[Value], schema: &Value) -> String {
    let documented = variants.iter().any(|v| v.get("description").is_some());
    let values: Vec<String> = variants
        .iter()
        .map(|v| {
            v.get("const")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    if !documented {
        return render_string_union(name, values, schema);
    }
    let mut out = doc_comment(schema, "");
    out.push_str(&format!("export type {name} =\n"));
    let last = variants.len().saturating_sub(1);
    for (i, v) in variants.iter().enumerate() {
        let sep = if i == last { ";" } else { "" };
        // The doc goes above its member, indented to line up with the `|`.
        out.push_str(&doc_comment(v, "  "));
        out.push_str(&format!("  | \"{}\"{sep}\n", values[i]));
    }
    out
}

/// An internally tagged enum: `{"type": "…", …fields}`.
///
/// A variant carrying a `$ref` beside its tag is serde's flattened newtype
/// (`#[serde(tag = "type")]` over `Variant(Inner)`), which is an intersection
/// here — the shape kotlinx could not express and had to hand-write.
fn render_tagged_union(name: &str, variants: &[Value], tag: &str, schema: &Value) -> String {
    let mut out = doc_comment(schema, "");
    out.push_str(&format!("export type {name} =\n"));
    let last = variants.len().saturating_sub(1);
    for (i, v) in variants.iter().enumerate() {
        let sep = if i == last { ";" } else { "" };
        let wire = v
            .get("properties")
            .and_then(|p| p.get(tag))
            .and_then(|t| t.get("const"))
            .and_then(Value::as_str)
            .unwrap_or_default();

        // Strip the tag; it is rendered as the literal member below.
        let mut without_tag = v.clone();
        if let Some(props) = without_tag
            .get_mut("properties")
            .and_then(Value::as_object_mut)
        {
            props.remove(tag);
        }
        if let Some(req) = without_tag
            .get_mut("required")
            .and_then(Value::as_array_mut)
        {
            req.retain(|r| r.as_str() != Some(tag));
        }

        out.push_str(&doc_comment(v, "  "));
        let fields = render_properties(&without_tag, "      ");
        let flattened = v.get("$ref").and_then(Value::as_str);

        let body = if fields.is_empty() {
            format!("{{ {}: \"{wire}\" }}", property_key(tag))
        } else {
            format!(
                "{{\n      {}: \"{wire}\";\n{}\n    }}",
                property_key(tag),
                fields.join("\n")
            )
        };
        match flattened {
            Some(reference) => {
                let inner = reference.rsplit('/').next().unwrap_or(reference);
                out.push_str(&format!("  | ({body} & {inner}){sep}\n"));
            }
            None => out.push_str(&format!("  | {body}{sep}\n")),
        }
    }
    out
}

/// An externally tagged enum: `{"Approved": {…}}`.
fn render_external_union(name: &str, variants: &[Value], schema: &Value) -> String {
    let mut out = doc_comment(schema, "");
    out.push_str(&format!("export type {name} =\n"));
    let last = variants.len().saturating_sub(1);
    for (i, v) in variants.iter().enumerate() {
        let sep = if i == last { ";" } else { "" };
        out.push_str(&doc_comment(v, "  "));
        let fields = render_properties(v, "      ");
        if fields.is_empty() {
            out.push_str(&format!("  | Record<string, never>{sep}\n"));
        } else {
            out.push_str(&format!("  | {{\n{}\n    }}{sep}\n", fields.join("\n")));
        }
    }
    out
}

fn render_interface(name: &str, schema: &Value) -> String {
    let mut out = doc_comment(schema, "");
    let lines = render_properties(schema, "  ");
    if lines.is_empty() {
        out.push_str(&format!("export type {name} = Record<string, never>;\n"));
        return out;
    }
    out.push_str(&format!("export interface {name} {{\n"));
    out.push_str(&lines.join("\n"));
    out.push_str("\n}\n");
    out
}

/// Render every type in `defs` as TypeScript, after `preamble`.
///
/// `preamble` is the caller's: the banner naming which Rust file to edit
/// differs per output, and the wire types open with two aliases that exist only
/// by convention. Every type to render must be in `defs` — a schema root that
/// lives outside `$defs` is the caller's to insert under the name it should
/// have, which is what the config schema does with `RawConfig`.
pub fn render(defs: &Map<String, Value>, preamble: &str) -> String {
    let mut out = String::from(preamble);

    // Sorted for a stable diff.
    let sorted: BTreeMap<_, _> = defs.iter().collect();
    let mut blocks: Vec<String> = Vec::new();

    for (name, schema) in sorted {
        if HAND_WRITTEN.contains(&name.as_str()) {
            continue;
        }

        if let Some(variants) = schema.get("oneOf").and_then(Value::as_array) {
            let all_string_consts = variants
                .iter()
                .all(|v| v.get("const").is_some() && v.get("properties").is_none());
            if all_string_consts {
                blocks.push(render_documented_string_union(name, variants, schema));
                continue;
            }
            if let Some(tag) = discriminator(variants) {
                blocks.push(render_tagged_union(name, variants, &tag, schema));
                continue;
            }
            if is_externally_tagged(variants) {
                blocks.push(render_external_union(name, variants, schema));
                continue;
            }
            panic!(
                "{name} is a `oneOf` shape this renderer does not handle; teach \
                 ts_types.rs about it or add it to HAND_WRITTEN with the reason"
            );
        }

        // An untagged union (`#[serde(untagged)]`): the variants are
        // alternatives with no tag to switch on, so this is a plain TS union.
        // Only the config schema produces these — `RawDays` is `"mon"` or
        // `["mon", "tue"]`, because the file format accepts both.
        if let Some(variants) = schema.get("anyOf").and_then(Value::as_array) {
            let parts: Vec<String> = variants.iter().map(|v| ts_type(v, "")).collect();
            blocks.push(format!(
                "{}export type {name} = {};\n",
                doc_comment(schema, ""),
                parts.join(" | ")
            ));
            continue;
        }

        if let Some(values) = schema.get("enum").and_then(Value::as_array) {
            let strings: Vec<String> = values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect();
            if strings.len() == values.len() {
                blocks.push(render_string_union(name, strings, schema));
                continue;
            }
        }

        match primitive(schema.get("type")).as_deref() {
            Some("object") => blocks.push(render_interface(name, schema)),
            // Newtypes over a primitive (`EntryId`, `GroupId`, `LimitSubject`)
            // are transparent on the wire, so they alias the underlying type
            // and call sites keep using plain strings. Falling through to the
            // object renderer instead produced `Record<string, never>`, which
            // type-checks and is wrong in every direction.
            Some(_) => blocks.push(format!(
                "{}export type {name} = {};\n",
                doc_comment(schema, ""),
                ts_type(schema, "")
            )),
            // Refuse rather than emit something plausible, the way the Kotlin
            // renderer does: a silently wrong mirror is the failure this whole
            // generator exists to prevent.
            None => panic!("{name} has a schema shape this renderer does not handle: {schema}"),
        }
    }

    for block in blocks {
        out.push('\n');
        out.push_str(&block);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn render_one(name: &str, schema: Value) -> String {
        let mut defs = Map::new();
        defs.insert(name.to_string(), schema);
        render(&defs, "")
    }

    /// `#[serde(untagged)]`, which only the config schema produces. Rendering
    /// it as anything else is how `RawDays` — `days = "mon"` or
    /// `days = ["mon", "tue"]` — would stop type-checking against the file
    /// format it describes.
    #[test]
    fn untagged_unions_render_as_a_plain_union() {
        let out = render_one(
            "RawDays",
            json!({
                "description": "Days specification",
                "anyOf": [{"type": "string"}, {"type": "array", "items": {"type": "string"}}],
            }),
        );
        assert!(
            out.contains("export type RawDays = string | string[];"),
            "{out}"
        );
        assert!(
            out.contains("Days specification"),
            "doc comment dropped: {out}"
        );
    }

    /// The two outputs differ only in their banner, so the preamble is the
    /// caller's and the renderer must not prepend one of its own.
    #[test]
    fn the_preamble_is_the_callers() {
        let mut defs = Map::new();
        defs.insert("Thing".to_string(), json!({"type": "string"}));
        let out = render(&defs, "// just this\n");
        assert!(out.starts_with("// just this\n"), "{out}");
        assert!(!out.contains("IsoTimestamp"), "wire aliases leaked: {out}");
    }
}
