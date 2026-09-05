//! The per-kind defaults the config editor needs, rendered from the Rust that
//! decides them.
//!
//! Some config fields have no fixed default: what an entry gets when it stays
//! silent depends on its `kind` (a book closes without confirming; nothing
//! else does). The daemon resolves that at
//! policy load, but the editor has to show an *unset* control as the value the
//! daemon will actually pick — so it needs the same answers, before any
//! resolution has happened.
//!
//! The first such answer was mirrored by hand in TypeScript, which is exactly
//! the shape of drift the generated wire types exist to prevent. So they are
//! generated here instead, by asking [`EntryKindTag`] itself, one row per kind.

use shepherd_api::EntryKindTag;

/// Render `kind-defaults.generated.ts`.
pub fn render() -> String {
    let mut out = String::new();
    out.push_str(
        "\
// GENERATED FILE — DO NOT EDIT BY HAND
//
// Rendered from `EntryKindTag`'s own answers in
// `crates/shepherd-api/src/types.rs` by
// `cargo run -p shepherd-wire-codegen --bin rpc-codegen`.
// Edit the Rust and re-run instead.
//
// What an entry gets when it leaves a field unset and its *kind* decides.
// The daemon resolves these at policy load; the editor needs them to show an
// unset control as the value the daemon will pick.

import type { RawEntryKind } from \"./config.generated\";

/** The defaults one kind supplies. */
export interface KindDefaults {
  /** Whether the HUD's close button confirms first. */
  confirm_on_close: boolean;
}

/** Every kind's defaults, keyed by the `kind.type` written in the config. */
export const KIND_DEFAULTS: Record<RawEntryKind[\"type\"], KindDefaults> = {
",
    );
    for tag in EntryKindTag::ALL {
        out.push_str(&format!(
            "  {}: {{ confirm_on_close: {} }},\n",
            tag.as_str(),
            tag.confirms_on_close_by_default(),
        ));
    }
    out.push_str("};\n");
    out
}
