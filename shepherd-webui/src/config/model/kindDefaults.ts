/**
 * What an entry inherits from its `kind` when it says nothing itself.
 *
 * The values are NOT written here: they come from
 * `kind-defaults.generated.ts`, rendered from `EntryKindTag`'s own answers in
 * `crates/lunchbox-api/src/types.rs`. This module is just the lookup, which
 * has to cope with an entry whose kind is not set yet — a state the editor has
 * and the daemon does not.
 *
 * Mirroring these by hand is what this replaces: the editor needs the same
 * answers the daemon resolves at policy load, and a copy of a rule is a copy
 * that drifts.
 */
import type { RawEntryKind, RawInputCompat } from "./config.generated";
import { KIND_DEFAULTS } from "./kind-defaults.generated";

/**
 * The row for this kind. A half-built entry with no kind yet gets `process`'s
 * answers, which are the unremarkable ones every kind but `ebook` shares.
 */
function defaultsFor(kind: RawEntryKind | undefined) {
  return KIND_DEFAULTS[kind?.type ?? "process"];
}

/**
 * Whether the HUD's "X" confirms before ending this activity, absent an
 * explicit `confirm_on_close`.
 */
export function confirmsOnCloseByDefault(kind: RawEntryKind | undefined): boolean {
  return defaultsFor(kind).confirm_on_close;
}

/** The input-compat sidecars this activity runs, absent an explicit list. */
export function defaultInputCompat(kind: RawEntryKind | undefined): RawInputCompat[] {
  return defaultsFor(kind).input_compat;
}
