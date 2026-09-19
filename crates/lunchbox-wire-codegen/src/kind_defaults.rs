//! The per-kind defaults the config editor needs, rendered from the Rust that
//! decides them.
//!
//! Some config fields have no fixed default: what an entry gets when it stays
//! silent depends on its `kind` (a book closes without confirming, and runs the
//! gamepad sidecar; nothing else does either). The daemon resolves that at
//! policy load, but the editor has to show an *unset* control as the value the
//! daemon will actually pick — so it needs the same answers, before any
//! resolution has happened.
//!
//! Those answers were mirrored by hand once, which is exactly the shape of
//! drift the generated wire types exist to prevent. So they are generated here
//! instead, by asking [`EntryKindTag`] itself, one row per kind.

use lunchbox_api::EntryKindTag;

/// Render `kind-defaults.generated.ts`.
pub fn render() -> String {
    let mut out = String::new();
    out.push_str(
        "\
// GENERATED FILE — DO NOT EDIT BY HAND
//
// Rendered from `EntryKindTag`'s own answers in
// `crates/lunchbox-api/src/types.rs` by
// `cargo run -p lunchbox-wire-codegen --bin rpc-codegen`.
// Edit the Rust and re-run instead.
//
// What an entry gets when it leaves a field unset and its *kind* decides.
// The daemon resolves these at policy load; the editor needs them to show an
// unset control as the value the daemon will pick.

import type { RawEntryKind, RawInputCompat } from \"./config.generated\";

/** The defaults one kind supplies. */
export interface KindDefaults {
  /** Whether the HUD's close button confirms first. */
  confirm_on_close: boolean;
  /** The input-compat sidecars the activity runs. */
  input_compat: RawInputCompat[];
}

/** Every kind's defaults, keyed by the `kind.type` written in the config. */
export const KIND_DEFAULTS: Record<RawEntryKind[\"type\"], KindDefaults> = {
",
    );
    for tag in EntryKindTag::ALL {
        let compat: Vec<String> = tag
            .default_input_compat()
            .iter()
            .map(|m| format!("\"{}\"", input_compat_wire_name(*m)))
            .collect();
        out.push_str(&format!(
            "  {}: {{ confirm_on_close: {}, input_compat: [{}] }},\n",
            tag.as_str(),
            tag.confirms_on_close_by_default(),
            compat.join(", "),
        ));
    }
    out.push_str("};\n");
    out
}

/// The config spelling of an input-compat mode, matching its serde rename.
///
/// Written out rather than derived so adding a mode without teaching the
/// generator about it fails to compile here, where it is one line, instead of
/// emitting a string the editor's own types reject.
fn input_compat_wire_name(mode: lunchbox_api::InputCompatMode) -> &'static str {
    use lunchbox_api::InputCompatMode as M;
    match mode {
        M::TouchToMouse => "touch_to_mouse",
        M::TabletToTouch => "tablet_to_touch",
        M::DisableTouch => "disable_touch",
        M::GamepadProductivity => "gamepad_productivity",
        M::GamepadGpd => "gamepad_gpd",
    }
}
