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

import type { RawEntryKind, RawInputCompat } from "./config.generated";

/** The defaults one kind supplies. */
export interface KindDefaults {
  /** Whether the HUD's close button confirms first. */
  confirm_on_close: boolean;
  /** The input-compat sidecars the activity runs. */
  input_compat: RawInputCompat[];
}

/** Every kind's defaults, keyed by the `kind.type` written in the config. */
export const KIND_DEFAULTS: Record<RawEntryKind["type"], KindDefaults> = {
  process: { confirm_on_close: true, input_compat: [] },
  snap: { confirm_on_close: true, input_compat: [] },
  steam: { confirm_on_close: true, input_compat: [] },
  flatpak: { confirm_on_close: true, input_compat: [] },
  vm: { confirm_on_close: true, input_compat: [] },
  media: { confirm_on_close: true, input_compat: [] },
  retroarch: { confirm_on_close: true, input_compat: [] },
  ebook: { confirm_on_close: false, input_compat: ["gamepad_productivity"] },
  custom: { confirm_on_close: true, input_compat: [] },
};
