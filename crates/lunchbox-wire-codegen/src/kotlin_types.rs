//! Render the wire-type JSON Schema as Kotlin.
//!
//! The companion's payload types were hand-written and drifted from the
//! device twice (see `wire_schema.rs`). This turns
//! [`crate::wire_schema::wire_schema`] into
//! `kotlinx.serialization` declarations so the mirrors can't fall behind
//! without CI noticing.
//!
//! Not everything is generated. Types whose serde shape has no direct
//! kotlinx equivalent stay hand-written and are listed in [`HAND_WRITTEN`];
//! the generator refuses to emit them rather than emitting something subtly
//! wrong.

use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// Types the generator deliberately skips, with the reason.
///
/// `Event` / `EventPayload`: the `state_changed` variant flattens a `$ref`
/// alongside its tag (`#[serde(tag = "type")]` over a newtype variant), which
/// kotlinx cannot express as a sealed subclass.
///
/// `LaunchOutcome`: externally tagged (`{"Approved": {…}}`), which needs the
/// bespoke `KSerializer` the companion already carries.
pub const HAND_WRITTEN: &[&str] = &["Event", "EventPayload", "LaunchOutcome"];

/// Rust type name -> Kotlin name, where the two differ.
fn kotlin_name(rust: &str) -> &str {
    match rust {
        // `std::time::Duration` serialises as `{secs, nanos}`; the companion
        // has always called that shape `DurationSecs`.
        "Duration" => "DurationSecs",
        other => other,
    }
}

/// A schema node's Kotlin type, plus whether it is nullable.
struct KType {
    name: String,
    nullable: bool,
}

impl KType {
    fn plain(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            nullable: false,
        }
    }
}

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

/// Map a property schema onto a Kotlin type.
fn kotlin_type(schema: &Value) -> KType {
    // A reference to another named type.
    if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
        let name = reference.rsplit('/').next().unwrap_or(reference);
        return KType::plain(kotlin_name(name));
    }

    // `Option<T>` where T is a named type renders as anyOf[$ref, null].
    if let Some(any_of) = schema.get("anyOf").and_then(Value::as_array) {
        let nullable = any_of
            .iter()
            .any(|v| v.get("type").and_then(Value::as_str) == Some("null"));
        if let Some(inner) = any_of
            .iter()
            .find(|v| v.get("type").and_then(Value::as_str) != Some("null"))
        {
            let mut t = kotlin_type(inner);
            t.nullable = t.nullable || nullable;
            return t;
        }
    }

    let ty = schema.get("type");
    let nullable = admits_null(ty);
    let base = match primitive(ty).as_deref() {
        Some("string") => match schema.get("format").and_then(Value::as_str) {
            // Typealiases to String, so callers keep using plain strings while
            // the intent stays readable.
            Some("date-time") => "IsoTimestamp".to_string(),
            Some("date") => "IsoDate".to_string(),
            _ => "String".to_string(),
        },
        // Every integer becomes Long: the Rust side mixes u32/u64/i64 and a
        // narrower Kotlin type would silently truncate.
        Some("integer") => "Long".to_string(),
        Some("number") => "Double".to_string(),
        Some("boolean") => "Boolean".to_string(),
        Some("array") => {
            let inner = schema
                .get("items")
                .map(kotlin_type)
                .unwrap_or_else(|| KType::plain("JsonElement"));
            format!("List<{}>", inner.name)
        }
        Some("object") => match schema.get("additionalProperties") {
            Some(v) if v.is_object() => format!("Map<String, {}>", kotlin_type(v).name),
            _ => "JsonElement".to_string(),
        },
        // An untyped node is `serde_json::Value` or similar.
        _ => "JsonElement".to_string(),
    };

    KType {
        name: base,
        nullable,
    }
}

/// The default expression for an optional property, or None if the property
/// must be required in Kotlin too.
fn default_for(ty: &KType, schema: &Value) -> Option<String> {
    if let Some(default) = schema.get("default") {
        match default {
            Value::Bool(b) => return Some(b.to_string()),
            Value::Number(n) => return Some(format!("{n}L")),
            Value::Null => return Some("null".into()),
            _ => {}
        }
    }
    if ty.nullable {
        return Some("null".into());
    }
    if ty.name.starts_with("List<") {
        return Some("emptyList()".into());
    }
    if ty.name.starts_with("Map<") {
        return Some("emptyMap()".into());
    }
    None
}

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

/// snake_case (the wire form) -> camelCase (Kotlin properties). The naming
/// strategy on `ShepherdJson` performs the same mapping at runtime.
///
/// Public because `rpc-codegen` renders RPC method and parameter names with it
/// too, and two implementations of this would be two chances to disagree.
pub fn camel(s: &str) -> String {
    let mut out = String::new();
    let mut upper = false;
    for c in s.chars() {
        if c == '_' {
            upper = true;
        } else if upper {
            out.extend(c.to_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// The tag property of an internally-tagged enum, if this `oneOf` is one.
fn discriminator(variants: &[Value]) -> Option<String> {
    let first = variants.first()?;
    let props = first.get("properties")?.as_object()?;
    let tag = props
        .iter()
        .find(|(_, v)| v.get("const").is_some())
        .map(|(k, _)| k.clone())?;
    // Every variant must carry the same tag for this to be a tagged enum.
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
            let ty = kotlin_type(prop);
            let optional = !required.contains(&name.as_str());
            let default = if optional {
                default_for(&ty, prop)
            } else {
                None
            };
            // An optional property with no natural default still has to be
            // constructible, so it becomes nullable.
            let (rendered_ty, default) = match (optional, default) {
                (true, Some(d)) => (
                    if ty.nullable {
                        format!("{}?", ty.name)
                    } else {
                        ty.name.clone()
                    },
                    Some(d),
                ),
                (true, None) => (format!("{}?", ty.name), Some("null".to_string())),
                (false, _) if ty.nullable => (format!("{}?", ty.name), Some("null".to_string())),
                (false, _) => (ty.name.clone(), None),
            };

            let doc = doc_comment(prop, indent);
            let assign = default.map(|d| format!(" = {d}")).unwrap_or_default();
            format!("{doc}{indent}val {}: {rendered_ty}{assign},", camel(name))
        })
        .collect()
}

fn render_data_class(name: &str, schema: &Value) -> String {
    let mut out = doc_comment(schema, "");
    out.push_str("@Serializable\n");
    out.push_str(&format!("data class {} (\n", kotlin_name(name)).replace(" (", "("));
    for line in render_properties(schema, "    ") {
        out.push_str(&line);
        out.push('\n');
    }
    out.push_str(")\n");
    out
}

fn render_string_enum(name: &str, variants: &[Value], schema: &Value) -> String {
    let cases: Vec<(String, String)> = variants
        .iter()
        .map(|v| {
            let wire = v.get("const").and_then(Value::as_str).unwrap_or_default();
            (wire.to_string(), doc_comment(v, "    "))
        })
        .collect();
    render_enum(name, &cases, schema)
}

/// A plain `{"type": "string", "enum": [...]}` — how schemars renders a
/// fieldless enum whose variants carry no docs.
fn render_plain_enum(name: &str, values: &[Value], schema: &Value) -> String {
    let cases: Vec<(String, String)> = values
        .iter()
        .map(|v| (v.as_str().unwrap_or_default().to_string(), String::new()))
        .collect();
    render_enum(name, &cases, schema)
}

/// The wire value a generated fallback variant carries.
///
/// Matches the `__unknown` the sealed-enum fallback uses, and cannot collide
/// with a real one: serde renders Rust variants in `snake_case`, which never
/// starts with an underscore.
const UNKNOWN_WIRE: &str = "__unknown";

/// Render a string-valued enum that tolerates a value it has never heard of.
///
/// `cases` is `(wire value, rendered doc comment)` in declaration order.
///
/// Forward compatibility, for the same reason the tagged enums above have an
/// `Unknown` variant: a device running a newer lunchboxd can send a value this
/// build predates. kotlinx's default enum serializer *throws* on one, and the
/// exception takes down the decode of the whole enclosing response — so an
/// older companion would fail to read a device's state entirely because one
/// field gained a variant. That is worst exactly when it matters: a new
/// `DiagnosticCode` is reported when something is already wrong with the
/// device.
///
/// `ignoreUnknownKeys` does not cover this. It forgives an unknown *key*; this
/// is a known key with an unknown *value*.
///
/// The fallback is a real variant so callers can match on it. An enum that
/// already has one (because the Rust type does, like `AudioOutputKind`) reuses
/// it rather than gaining a second — which also keeps any exhaustive `when`
/// over it compiling.
fn render_enum(name: &str, cases: &[(String, String)], schema: &Value) -> String {
    let kname = kotlin_name(name);
    let existing_fallback = cases
        .iter()
        .find(|(wire, _)| constant_name(wire) == "UNKNOWN")
        .map(|(wire, _)| constant_name(wire));
    let fallback = existing_fallback
        .clone()
        .unwrap_or_else(|| "UNKNOWN".to_string());

    let mut out = doc_comment(schema, "");
    out.push_str(&format!(
        "@Serializable(with = {kname}.Serializer::class)\n"
    ));
    out.push_str(&format!("enum class {kname}(val wire: String) {{\n"));
    for (wire, docs) in cases {
        out.push_str(docs);
        out.push_str(&format!("    {}(\"{wire}\"),\n", constant_name(wire)));
    }
    if existing_fallback.is_none() {
        out.push_str(&format!(
            "    /**\n     * A [{kname}] this build doesn't know about.\n     *\n\
             \x20    * A newer device degrades to this one value instead of failing the\n\
             \x20    * decode of everything around it. Never sent by a device.\n     */\n"
        ));
        out.push_str(&format!("    {fallback}(\"{UNKNOWN_WIRE}\"),\n"));
    }
    // Replace the trailing comma of the last constant with the semicolon Kotlin
    // needs before members.
    if out.ends_with(",\n") {
        out.truncate(out.len() - 2);
        out.push_str(";\n");
    }
    out.push_str(&format!(
        "\n    internal object Serializer : KSerializer<{kname}> {{\n\
         \x20       override val descriptor: SerialDescriptor =\n\
         \x20           PrimitiveSerialDescriptor(\"{kname}\", PrimitiveKind.STRING)\n\
         \x20       override fun serialize(encoder: Encoder, value: {kname}) =\n\
         \x20           encoder.encodeString(value.wire)\n\
         \x20       override fun deserialize(decoder: Decoder): {kname} {{\n\
         \x20           val wire = decoder.decodeString()\n\
         \x20           return entries.firstOrNull {{ it.wire == wire }} ?: {fallback}\n\
         \x20       }}\n\
         \x20   }}\n"
    ));
    out.push_str("}\n");
    out
}

/// Kotlin enum-constant name for a snake_case wire value.
///
/// A wire value may start with a digit (`MediaQuality`'s `1080p`), which is not
/// a legal Kotlin identifier, so those are prefixed with `Q_` — the value
/// itself still travels verbatim in `@SerialName`. Non-alphanumerics become
/// underscores for the same reason.
fn constant_name(wire: &str) -> String {
    let mut out: String = wire
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect::<String>()
        .to_uppercase();
    if out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert_str(0, "Q_");
    }
    out
}

fn render_sealed(name: &str, variants: &[Value], tag: &str, schema: &Value) -> String {
    let kname = kotlin_name(name);
    let mut out = doc_comment(schema, "");
    out.push_str("@Serializable\n");
    out.push_str(&format!("@JsonClassDiscriminator(\"{tag}\")\n"));
    out.push_str(&format!("sealed interface {kname} {{\n"));

    for v in variants {
        let wire = v
            .get("properties")
            .and_then(|p| p.get(tag))
            .and_then(|t| t.get("const"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        // Strip the tag itself; it is carried by @SerialName, not a field.
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

        let variant = pascal(wire);
        let fields = render_properties(&without_tag, "        ");
        out.push_str(&doc_comment(v, "    "));
        out.push_str("    @Serializable\n");
        out.push_str(&format!("    @SerialName(\"{wire}\")\n"));
        if fields.is_empty() {
            out.push_str(&format!("    data object {variant} : {kname}\n\n"));
        } else {
            out.push_str(&format!("    data class {variant}(\n"));
            for f in fields {
                out.push_str(&f);
                out.push('\n');
            }
            out.push_str(&format!("    ) : {kname}\n\n"));
        }
    }

    // Forward compatibility: a device running a newer lunchboxd can send a
    // variant this build has never heard of. Without a fallback kotlinx throws
    // and takes the whole enclosing response down with it — which is exactly
    // how four missing ReasonCode variants broke the companion's entry list.
    out.push_str(&format!(
        "    /**\n     * A [{kname}] this build doesn't know about.\n     *\n\
         \x20    * Registered as the polymorphic default in `ShepherdWireModule`, so a\n\
         \x20    * newer device degrades this one value instead of failing the decode of\n\
         \x20    * everything around it.\n     */\n"
    ));
    out.push_str("    @Serializable\n    @SerialName(\"__unknown\")\n");
    out.push_str(&format!(
        "    data class Unknown(val {tag}: String? = null) : {kname}\n"
    ));
    out.push_str("}\n");
    out
}

fn pascal(s: &str) -> String {
    s.split('_')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut c = p.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

/// Render every generatable type in the schema, plus the serializers module.
pub fn render(defs: &Map<String, Value>) -> String {
    let mut out = String::new();
    out.push_str("// GENERATED FILE — DO NOT EDIT BY HAND\n//\n");
    out.push_str("// Rendered from the Rust wire types by\n");
    out.push_str("// `cargo run -p lunchbox-wire-codegen --bin rpc-codegen`.\n");
    out.push_str("// Edit `crates/lunchbox-api/src/types.rs` and re-run instead.\n//\n");
    out.push_str("// Helper affordances (extension properties, custom serializers, and the\n");
    out.push_str("// types listed as hand-written in `kotlin_types.rs`) live in\n");
    out.push_str("// `Models.kt` alongside this file.\n\n");
    out.push_str("@file:OptIn(ExperimentalSerializationApi::class)\n\n");
    out.push_str("package com.armeafamily.shepherd.companion.domain\n\n");
    out.push_str("import kotlinx.serialization.ExperimentalSerializationApi\n");
    out.push_str("import kotlinx.serialization.KSerializer\n");
    out.push_str("import kotlinx.serialization.SerialName\n");
    out.push_str("import kotlinx.serialization.Serializable\n");
    out.push_str("import kotlinx.serialization.descriptors.PrimitiveKind\n");
    out.push_str("import kotlinx.serialization.descriptors.PrimitiveSerialDescriptor\n");
    out.push_str("import kotlinx.serialization.descriptors.SerialDescriptor\n");
    out.push_str("import kotlinx.serialization.encoding.Decoder\n");
    out.push_str("import kotlinx.serialization.encoding.Encoder\n");
    out.push_str("import kotlinx.serialization.json.JsonClassDiscriminator\n");
    out.push_str("import kotlinx.serialization.json.JsonElement\n");
    out.push_str("import kotlinx.serialization.modules.SerializersModule\n");
    out.push_str("import kotlinx.serialization.modules.polymorphic\n\n");

    // Sorted for a stable diff.
    let sorted: BTreeMap<_, _> = defs.iter().collect();
    let mut sealed_types: Vec<String> = Vec::new();
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
                blocks.push(render_string_enum(name, variants, schema));
                continue;
            }
            match discriminator(variants) {
                Some(tag) => {
                    // A variant that carries a `$ref` beside its tag is a
                    // flattened newtype; kotlinx has no equivalent, so refuse
                    // rather than emit something that decodes wrongly.
                    if variants.iter().any(|v| v.get("$ref").is_some()) {
                        panic!(
                            "{name} has a flattened variant; add it to HAND_WRITTEN \
                             in kotlin_types.rs"
                        );
                    }
                    sealed_types.push(kotlin_name(name).to_string());
                    blocks.push(render_sealed(name, variants, &tag, schema));
                }
                None => panic!(
                    "{name} is an untagged or externally-tagged enum with no Kotlin \
                     equivalent; add it to HAND_WRITTEN in kotlin_types.rs"
                ),
            }
            continue;
        }

        // A fieldless enum: schemars renders it as an `enum` array rather
        // than a `oneOf` of consts when no variant carries a doc comment.
        if let Some(values) = schema.get("enum").and_then(Value::as_array) {
            blocks.push(render_plain_enum(name, values, schema));
            continue;
        }

        match primitive(schema.get("type")).as_deref() {
            // Newtypes over a string (IDs, LimitSubject) are transparent on
            // the wire; a typealias keeps call sites using plain Strings.
            Some("string") => {
                blocks.push(format!(
                    "{}typealias {} = String\n",
                    doc_comment(schema, ""),
                    kotlin_name(name)
                ));
            }
            Some("object") => blocks.push(render_data_class(name, schema)),
            _ => panic!("{name} has an unsupported schema shape: {schema}"),
        }
    }

    out.push_str("/** ISO-8601 timestamp with offset, e.g. \"2026-06-21T18:05:00-04:00\". */\n");
    out.push_str("typealias IsoTimestamp = String\n\n");
    out.push_str("/** ISO-8601 local date, e.g. \"2026-06-21\". */\n");
    out.push_str("typealias IsoDate = String\n\n");
    out.push_str(&blocks.join("\n"));

    // One module registering every tagged enum's fallback.
    out.push('\n');
    out.push_str("/**\n * Polymorphic defaults for every tagged enum above.\n *\n");
    out.push_str(" * Installed on `ShepherdJson`; without it an unrecognised discriminator\n");
    out.push_str(" * throws and fails the decode of the entire enclosing response.\n */\n");
    out.push_str("val ShepherdWireModule: SerializersModule = SerializersModule {\n");
    for t in &sealed_types {
        out.push_str(&format!(
            "    polymorphic({t}::class) {{ defaultDeserializer {{ {t}.Unknown.serializer() }} }}\n"
        ));
    }
    out.push_str("}\n");

    out
}
