/**
 * Defaults an entry inherits from its kind when it says nothing itself.
 *
 * These mirror `EntryKind`'s own answers in `crates/shepherd-api/src/types.rs`.
 * The editor needs them so a control that is *unset* shows what the daemon will
 * actually do, rather than a hard-coded guess that is wrong for one kind.
 */
import type { RawEntryKind } from "./config.generated";

/**
 * Whether the HUD's "X" confirms before ending this activity, absent an
 * explicit `confirm_on_close`.
 *
 * The prompt guards unsaved work. A book has none — its page is written on the
 * way out — so a reading activity closes on one tap. See
 * `EntryKind::confirms_on_close_by_default`.
 */
export function confirmsOnCloseByDefault(kind: RawEntryKind | undefined): boolean {
  return kind?.type !== "ebook";
}
