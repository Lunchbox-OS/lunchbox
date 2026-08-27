//! Read `RPC_SCHEMA_JSON` (emitted by the `#[management_rpc]` macro
//! on the `ManagementService` trait) and produce the three artifacts
//! external consumers rely on:
//!
//! - `docs/rpc-schema.json` — pretty-printed schema, committed to
//!   the repo so it's diffable in PRs and available to future
//!   codegen tools.
//! - `companion-android/app/src/main/kotlin/com/armeafamily/shepherd/companion/ble/RpcMethods.kt`
//!   — typed method-name constants for the Kotlin companion. Kills
//!   the raw method-name string literals scattered through
//!   `ManagementClient.kt`.
//! - `shepherd-webui/src/config/model/config.generated.ts` — TypeScript
//!   mirrors of the `config.toml` schema, for the config editor.
//! - `companion-android/.../companion/domain/RpcParams.generated.kt` —
//!   a params builder per RPC, so the companion stops spelling wire
//!   param keys as string literals.
//! - `shepherd-webui/src/api/rpc-methods.generated.ts` — the same
//!   for the TypeScript web UI: a union of all method names, plus the
//!   params and result type of each one.
//! - `shepherd-webui/src/api/wire-types.generated.ts` — the payload
//!   types for the web UI, the TypeScript counterpart of
//!   `WireTypes.generated.kt`.
//!
//! Run as `cargo run -p shepherd-wire-codegen --bin rpc-codegen`
//! from the repo root. The binary is deterministic: same schema in,
//! same files out, so it's safe to invoke from a pre-commit hook or
//! a CI check that fails on drift.

use serde::Deserialize;
use serde_json::{Map, Value};
use shepherd_wire_codegen::rust_types::RustType;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
struct Schema {
    methods: Vec<Method>,
}

#[derive(Debug, Deserialize)]
struct Method {
    name: String,
    #[serde(default)]
    params: Vec<Param>,
    result: Result_,
}

/// One RPC parameter. `ty` is the source text of the type from the trait
/// signature; [`RustType::parse`] turns it into something renderable.
///
/// `required` is not the same as non-`Option`: the macro sets it false for a
/// parameter the caller may omit entirely, which is usually but not always an
/// `Option<T>` (`max_volume: Option<u8>` is both).
#[derive(Debug, Deserialize)]
struct Param {
    name: String,
    required: bool,
    #[serde(rename = "type")]
    ty: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename = "Result")]
struct Result_ {
    #[serde(rename = "type")]
    ty: String,
    #[serde(default)]
    wrap_field: Option<String>,
}

impl Param {
    fn parsed(&self) -> RustType {
        RustType::parse(&self.ty)
    }

    /// True when the value may be absent or null, either because the type is
    /// an `Option` or because the caller may leave it out.
    fn nullable(&self) -> bool {
        !self.required || matches!(self.parsed(), RustType::Option(_))
    }
}

impl Method {
    /// The result as it appears on the wire: the bare type, or the single-key
    /// object `#[rpc(wrap_result = "...")]` puts it in.
    fn wire_result_ts(&self) -> String {
        let ty = RustType::parse(&self.result.ty).ts();
        match &self.result.wrap_field {
            Some(field) => format!("{{ {field}: {ty} }}"),
            None => ty,
        }
    }
}

fn main() -> anyhow::Result<()> {
    let schema: Schema = serde_json::from_str(shepherd_management::RPC_SCHEMA_JSON)?;
    let pretty = serde_json::to_string_pretty(&serde_json::from_str::<Value>(
        shepherd_management::RPC_SCHEMA_JSON,
    )?)?;

    // Two modes:
    //
    // - Normal (`SHEPHERD_RPC_CODEGEN_OUT` unset): overwrite the
    //   canonical locations in the repo, resolved from `CARGO_MANIFEST_DIR`
    //   so the binary doesn't care what the caller's cwd is.
    // - Test mode (`SHEPHERD_RPC_CODEGEN_OUT=<dir>`): write flattened
    //   filenames into <dir>. The drift-check test uses this to compare
    //   against the checked-in copies without racing against a concurrent
    //   `cargo run`.
    let outputs: [(PathBuf, String); 7] = if let Ok(dir) = std::env::var("SHEPHERD_RPC_CODEGEN_OUT")
    {
        let base = PathBuf::from(dir);
        [
            (base.join("rpc-schema.json"), format!("{pretty}\n")),
            (base.join("RpcMethods.kt"), render_kotlin(&schema)),
            (
                base.join("RpcParams.generated.kt"),
                render_kotlin_params(&schema),
            ),
            (base.join("rpc-methods.generated.ts"), render_ts(&schema)),
            (base.join("WireTypes.generated.kt"), render_wire_types()),
            (base.join("wire-types.generated.ts"), render_wire_types_ts()),
            (base.join("config.generated.ts"), render_config_types()),
        ]
    } else {
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let repo = manifest_dir
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| anyhow::anyhow!("no repo root above CARGO_MANIFEST_DIR"))?
            .to_path_buf();
        [
            (
                repo.join("docs/rpc-schema.json"),
                format!("{pretty}\n"),
            ),
            (
                repo.join("companion-android/app/src/main/kotlin/com/armeafamily/shepherd/companion/ble/RpcMethods.kt"),
                render_kotlin(&schema),
            ),
            (
                repo.join("companion-android/app/src/main/kotlin/com/armeafamily/shepherd/companion/domain/RpcParams.generated.kt"),
                render_kotlin_params(&schema),
            ),
            (
                repo.join("shepherd-webui/src/api/rpc-methods.generated.ts"),
                render_ts(&schema),
            ),
            (
                repo.join("companion-android/app/src/main/kotlin/com/armeafamily/shepherd/companion/domain/WireTypes.generated.kt"),
                render_wire_types(),
            ),
            (
                repo.join("shepherd-webui/src/api/wire-types.generated.ts"),
                render_wire_types_ts(),
            ),
            (
                repo.join("shepherd-webui/src/config/model/config.generated.ts"),
                render_config_types(),
            ),
        ]
    };

    for (path, contents) in &outputs {
        fs::write(path, contents)?;
        println!("wrote {}", path.display());
    }

    Ok(())
}

/// The banner and by-convention aliases the wire mirror opens with.
const WIRE_TS_PREAMBLE: &str = "\
// GENERATED FILE — DO NOT EDIT BY HAND
//
// Rendered from the Rust wire types by
// `cargo run -p shepherd-wire-codegen --bin rpc-codegen`.
// Edit `crates/shepherd-api/src/types.rs` and re-run instead.
//
// Property names are the wire form (snake_case), because that is what the
// daemon sends and nothing renames them in transit.

/** An RFC 3339 timestamp. A `string`; the alias records the intent. */
export type IsoTimestamp = string;

/** A calendar date, `YYYY-MM-DD`. */
export type IsoDate = string;
";

/// TypeScript mirrors of the payload types, from the same wire JSON Schema.
fn render_wire_types_ts() -> String {
    let schema = shepherd_wire_codegen::wire_schema::wire_schema();
    shepherd_wire_codegen::ts_types::render(&schema, WIRE_TS_PREAMBLE)
}

const CONFIG_TS_PREAMBLE: &str = "\
// GENERATED FILE — DO NOT EDIT BY HAND
//
// Rendered from `crates/shepherd-config/src/schema.rs` by
// `cargo run -p shepherd-wire-codegen --bin rpc-codegen`.
// Edit the Rust types and re-run instead.
//
// These describe the *projection* the editor renders — what serde produces
// when the daemon's parser reads a file — not TOML syntax. Where the file
// format accepts a shorthand (`input_compat = \"touch_to_mouse\"` as well as a
// list), the projection always carries the canonical form shown here.
";

/// TypeScript mirrors of the config schema, for the web config editor.
fn render_config_types() -> String {
    let schema = shepherd_wire_codegen::config_schema::config_schema();
    let mut value = serde_json::to_value(&schema).expect("config schema serializes");
    let object = value.as_object_mut().expect("config schema is an object");

    // `schemars` puts every named type in `$defs` and leaves the root — the
    // `RawConfig` struct itself — inline. The renderer only walks a map, so
    // lift the root into it under its own name; taking `$defs` out first keeps
    // it from being rendered as a property of itself.
    let mut defs = match object.remove("$defs") {
        Some(Value::Object(defs)) => defs,
        _ => Map::new(),
    };
    defs.insert("RawConfig".to_string(), value);

    shepherd_wire_codegen::ts_types::render(&defs, CONFIG_TS_PREAMBLE)
}

/// Kotlin mirrors of the payload types, rendered from the wire JSON Schema.
fn render_wire_types() -> String {
    shepherd_wire_codegen::kotlin_types::render(&shepherd_wire_codegen::wire_schema::wire_schema())
}

// ---------------------------------------------------------------------------
// Kotlin rendering
// ---------------------------------------------------------------------------

/// Emit method-name constants plus a small helper mapping each name
/// back onto the wrap-field (if any) so the Kotlin companion doesn't
/// duplicate the `{"deleted": ...}` unwrap knowledge that lives on the
/// trait.
fn render_kotlin(schema: &Schema) -> String {
    let mut out = String::new();
    out.push_str("// GENERATED FILE — DO NOT EDIT BY HAND\n");
    out.push_str("//\n");
    out.push_str("// Run `cargo run -p shepherd-wire-codegen --bin rpc-codegen`\n");
    out.push_str("// after changing the `ManagementService` trait in\n");
    out.push_str("// `crates/shepherd-management/src/service.rs`.\n\n");
    out.push_str("package com.armeafamily.shepherd.companion.ble\n\n");
    out.push_str("/**\n");
    out.push_str(" * Wire-name constants for every RPC exposed by the shepherd device.\n");
    out.push_str(" * Mirrors the trait annotated with `#[management_rpc]` on the Rust side,\n");
    out.push_str(" * generated from that trait's `RPC_SCHEMA_JSON` so a drift between the\n");
    out.push_str(" * two sides is a CI failure, not a silent runtime miss.\n");
    out.push_str(" */\n");
    out.push_str("object RpcMethods {\n");
    for m in &schema.methods {
        let const_name = m.name.to_ascii_uppercase();
        out.push_str(&format!(
            "    const val {const_name}: String = \"{}\"\n",
            m.name
        ));
    }
    out.push('\n');
    out.push_str("    /**\n");
    out.push_str("     * For methods whose result on the wire is `{\"<field>\": <value>}`\n");
    out.push_str("     * (via `#[rpc(wrap_result = \"<field>\")]`), the field name to unwrap.\n");
    out.push_str("     * `null` for methods whose result is a bare value or a full object.\n");
    out.push_str("     */\n");
    out.push_str("    fun wrapField(method: String): String? = when (method) {\n");
    for m in &schema.methods {
        if let Some(field) = &m.result.wrap_field {
            out.push_str(&format!("        \"{}\" -> \"{}\"\n", m.name, field));
        }
    }
    out.push_str("        else -> null\n");
    out.push_str("    }\n");
    out.push_str("}\n");
    out
}

/// Emit one params builder per RPC, so the companion stops spelling wire keys
/// as string literals.
///
/// Lives in the `domain` package beside the wire types rather than in `ble`
/// beside [`render_kotlin`]'s method names: the builders reference the payload
/// enums (`StopMode`, `WindowAction`), and `domain` already depends on `ble`
/// for `ShepherdJson`.
fn render_kotlin_params(schema: &Schema) -> String {
    let defs = shepherd_wire_codegen::wire_schema::wire_schema();

    let mut out = String::new();
    out.push_str("// GENERATED FILE — DO NOT EDIT BY HAND\n");
    out.push_str("//\n");
    out.push_str("// Run `cargo run -p shepherd-wire-codegen --bin rpc-codegen`\n");
    out.push_str("// after changing the `ManagementService` trait in\n");
    out.push_str("// `crates/shepherd-management/src/service.rs`.\n\n");
    out.push_str("package com.armeafamily.shepherd.companion.domain\n\n");
    out.push_str("import com.armeafamily.shepherd.companion.ble.ShepherdJson\n");
    out.push_str("import kotlinx.serialization.json.JsonNull\n");
    out.push_str("import kotlinx.serialization.json.JsonObject\n");
    out.push_str("import kotlinx.serialization.json.JsonPrimitive\n");
    out.push_str("import kotlinx.serialization.json.buildJsonObject\n\n");
    out.push_str("/**\n");
    out.push_str(" * The params object for every RPC the device speaks, built from the\n");
    out.push_str(" * `ManagementService` trait's own signatures.\n");
    out.push_str(" *\n");
    out.push_str(" * `ManagementClient` used to spell these keys as string literals, which\n");
    out.push_str(" * left a renamed parameter compiling on both sides and failing at run\n");
    out.push_str(" * time — the same gap that let the hand-written payload mirrors drift\n");
    out.push_str(" * twice before they were generated.\n");
    out.push_str(" *\n");
    out.push_str(" * A parameter the caller may omit is sent explicitly as `null`, which the\n");
    out.push_str(" * daemon reads the same way as an absent key.\n");
    out.push_str(" */\n");
    out.push_str("object RpcParams {\n");

    for (i, m) in schema.methods.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let fn_name = shepherd_wire_codegen::kotlin_types::camel(&m.name);

        if m.params.is_empty() {
            out.push_str(&format!(
                "    /** Params for `{}`, which takes none. */\n",
                m.name
            ));
            out.push_str(&format!(
                "    fun {fn_name}(): JsonObject = JsonObject(emptyMap())\n"
            ));
            continue;
        }

        let args: Vec<String> = m
            .params
            .iter()
            .map(|p| {
                let name = shepherd_wire_codegen::kotlin_types::camel(&p.name);
                let mut ty = p.parsed().kotlin();
                if p.nullable() && !ty.ends_with('?') {
                    ty.push('?');
                }
                let default = if p.nullable() { " = null" } else { "" };
                format!("{name}: {ty}{default}")
            })
            .collect();

        out.push_str(&format!("    /** Params for `{}`. */\n", m.name));
        out.push_str(&format!(
            "    fun {fn_name}({}): JsonObject = buildJsonObject {{\n",
            args.join(", ")
        ));
        for p in &m.params {
            let name = shepherd_wire_codegen::kotlin_types::camel(&p.name);
            out.push_str(&format!(
                "        put(\"{}\", {})\n",
                p.name,
                kotlin_json_value(p, &name, &defs)
            ));
        }
        out.push_str("    }\n");
    }

    out.push_str("}\n");
    out
}

/// The expression putting one parameter's value on the wire.
fn kotlin_json_value(param: &Param, expr: &str, defs: &Map<String, Value>) -> String {
    let ty = param.parsed();
    // The `Option` is carried by `nullable()`; encode what is inside it.
    let inner = match &ty {
        RustType::Option(inner) => inner.as_ref(),
        other => other,
    };

    let encode = |value: &str| -> String {
        match inner {
            RustType::Vec(_) => panic!(
                "{}: a `Vec` parameter has no JsonPrimitive form; teach \
                 kotlin_json_value to encode it",
                param.name
            ),
            RustType::Named(name) if is_kotlin_enum(name, defs) => {
                format!("ShepherdJson.encodeToJsonElement({name}.serializer(), {value})")
            }
            // Everything else is a primitive, or a newtype over a string that
            // `kotlin_types` renders as a `typealias` to `String`.
            _ => format!("JsonPrimitive({value})"),
        }
    };

    if param.nullable() {
        format!("{expr}?.let {{ {} }} ?: JsonNull", encode("it"))
    } else {
        encode(expr)
    }
}

/// Whether the wire schema describes `name` as an enum, which `kotlin_types`
/// renders as an `enum class` needing its serializer — as opposed to a newtype
/// over a string, which it renders as a transparent `typealias`.
fn is_kotlin_enum(name: &str, defs: &Map<String, Value>) -> bool {
    let Some(def) = defs.get(name) else {
        return false;
    };
    def.get("enum").is_some() || def.get("oneOf").is_some()
}

// ---------------------------------------------------------------------------
// TypeScript rendering
// ---------------------------------------------------------------------------

/// Emit the union of method-name string literals, the params object each
/// method takes, and the result each one answers with.
///
/// Together those make `call(method, params)` in `src/api/client.ts` checkable
/// end to end: before this, both halves were hand-written there, so a renamed
/// parameter compiled fine on both sides and failed at runtime.
fn render_ts(schema: &Schema) -> String {
    let mut imports: BTreeSet<String> = BTreeSet::new();
    for m in &schema.methods {
        for p in &m.params {
            p.parsed().imports(&mut imports);
        }
        RustType::parse(&m.result.ty).imports(&mut imports);
    }

    let mut out = String::new();
    out.push_str("// GENERATED FILE — DO NOT EDIT BY HAND\n");
    out.push_str("//\n");
    out.push_str("// Run `cargo run -p shepherd-wire-codegen --bin rpc-codegen`\n");
    out.push_str("// after changing the `ManagementService` trait in\n");
    out.push_str("// `crates/shepherd-management/src/service.rs`.\n\n");

    out.push_str("import type {\n");
    for name in &imports {
        out.push_str(&format!("  {name},\n"));
    }
    out.push_str("} from \"./wire-types.generated\";\n\n");

    out.push_str("/**\n");
    out.push_str(" * Every RPC method the shepherd device speaks. The web-ui client is\n");
    out.push_str(" * REST-shaped and doesn't dispatch by name, but references such as\n");
    out.push_str(" * feature-flag names or telemetry event names benefit from a compile-time\n");
    out.push_str(" * check that the string matches a real RPC.\n");
    out.push_str(" */\n");
    out.push_str("export type RpcMethod =\n");
    let last = schema.methods.len() - 1;
    for (i, m) in schema.methods.iter().enumerate() {
        let sep = if i == last { ";" } else { "" };
        out.push_str(&format!("  | \"{}\"{sep}\n", m.name));
    }
    out.push('\n');

    out.push_str(
        "/** Wrap-field lookup for methods whose wire result is `{\"<field>\": <value>}`. */\n",
    );
    out.push_str("export const RPC_WRAP_FIELDS: Partial<Record<RpcMethod, string>> = {\n");
    for m in &schema.methods {
        if let Some(field) = &m.result.wrap_field {
            out.push_str(&format!("  \"{}\": \"{}\",\n", m.name, field));
        }
    }
    out.push_str("};\n\n");

    out.push_str("/**\n");
    out.push_str(" * The params object each method takes.\n");
    out.push_str(" *\n");
    out.push_str(" * Keys are the wire form (snake_case), because that is what the daemon\n");
    out.push_str(" * deserializes into the trait method's arguments. An optional key may be\n");
    out.push_str(" * left out entirely; `JSON.stringify` drops an `undefined` value, which\n");
    out.push_str(" * the daemon reads the same way as an absent one.\n");
    out.push_str(" */\n");
    out.push_str("export interface RpcParamsMap {\n");
    for m in &schema.methods {
        if m.params.is_empty() {
            out.push_str(&format!("  \"{}\": Record<string, never>;\n", m.name));
            continue;
        }
        out.push_str(&format!("  \"{}\": {{\n", m.name));
        for p in &m.params {
            let opt = if p.required { "" } else { "?" };
            out.push_str(&format!("    {}{opt}: {};\n", p.name, p.parsed().ts()));
        }
        out.push_str("  };\n");
    }
    out.push_str("}\n\n");
    out.push_str("export type RpcParams<M extends RpcMethod> = RpcParamsMap[M];\n\n");

    out.push_str("/**\n");
    out.push_str(" * What each method answers with, as it arrives on the wire.\n");
    out.push_str(" *\n");
    out.push_str(" * Methods carrying a `RPC_WRAP_FIELDS` entry are typed as the wrapping\n");
    out.push_str(" * object rather than the value inside it, so the type matches the bytes\n");
    out.push_str(" * and the unwrap stays visible at the call site.\n");
    out.push_str(" */\n");
    out.push_str("export interface RpcResultMap {\n");
    for m in &schema.methods {
        out.push_str(&format!("  \"{}\": {};\n", m.name, m.wire_result_ts()));
    }
    out.push_str("}\n\n");
    out.push_str("export type RpcResult<M extends RpcMethod> = RpcResultMap[M];\n");
    out
}
