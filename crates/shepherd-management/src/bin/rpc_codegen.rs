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
//! - `shepherd-webui/src/api/rpc-methods.generated.ts` — the same
//!   for the TypeScript web UI: a union type of all method names
//!   plus a per-method result-type helper (only string-level today,
//!   full type mapping is a follow-on).
//!
//! Run as `cargo run -p shepherd-management --bin rpc-codegen`
//! from the repo root. The binary is deterministic: same schema in,
//! same files out, so it's safe to invoke from a pre-commit hook or
//! a CI check that fails on drift.

use serde::Deserialize;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
struct Schema {
    methods: Vec<Method>,
}

/// Only the fields the renderers actually read are pulled off the
/// blob. `params` types are exposed to future type-mapping codegen but
/// today the Kotlin and TypeScript emitters only need the method name
/// and the wrap-field hint.
#[derive(Debug, Deserialize)]
struct Method {
    name: String,
    result: Result_,
}

#[derive(Debug, Deserialize)]
#[serde(rename = "Result")]
struct Result_ {
    #[serde(default)]
    wrap_field: Option<String>,
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
    let outputs: [(PathBuf, String); 3] = if let Ok(dir) = std::env::var("SHEPHERD_RPC_CODEGEN_OUT")
    {
        let base = PathBuf::from(dir);
        [
            (base.join("rpc-schema.json"), format!("{pretty}\n")),
            (base.join("RpcMethods.kt"), render_kotlin(&schema)),
            (base.join("rpc-methods.generated.ts"), render_ts(&schema)),
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
                repo.join("shepherd-webui/src/api/rpc-methods.generated.ts"),
                render_ts(&schema),
            ),
        ]
    };

    for (path, contents) in &outputs {
        fs::write(path, contents)?;
        println!("wrote {}", path.display());
    }

    Ok(())
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
    out.push_str("// Run `cargo run -p shepherd-management --bin rpc-codegen`\n");
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

// ---------------------------------------------------------------------------
// TypeScript rendering
// ---------------------------------------------------------------------------

/// Emit the union of method-name string literals plus a small
/// `wrapField` lookup. The web-ui is REST-shaped today, so we don't
/// generate call wrappers — this file exists so a change to the trait
/// forces the TS side to acknowledge new/renamed methods at build
/// time.
fn render_ts(schema: &Schema) -> String {
    let mut out = String::new();
    out.push_str("// GENERATED FILE — DO NOT EDIT BY HAND\n");
    out.push_str("//\n");
    out.push_str("// Run `cargo run -p shepherd-management --bin rpc-codegen`\n");
    out.push_str("// after changing the `ManagementService` trait in\n");
    out.push_str("// `crates/shepherd-management/src/service.rs`.\n\n");
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
    out.push_str("};\n");
    out
}
