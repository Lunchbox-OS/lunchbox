/**
 * Which subjects a token gate is allowed to earn from.
 *
 * This mirrors `validate_tokens` in `crates/lunchbox-config/src/validation.rs`,
 * which rejects four shapes of "a thing unlocking itself". Encoding them here
 * is what stops the picker offering a choice the daemon would refuse — but it
 * also means the two can drift, so the rules are stated once, here, and the
 * paired Rust tests in `tests/group_tokens.rs` assert the validator still
 * rejects exactly what this excludes.
 */
import type { Subject } from "../doc/patches";
import type { RawConfig } from "./config.generated";

export const GROUP_PREFIX = "group:";

export interface TokenSource {
  /** The wire value: an entry id, or `group:<id>`. */
  value: string;
  label: string;
  kind: "entry" | "group";
}

/**
 * Options for a gate owned by `subject`, categories first — which is what
 * MUI's `groupBy` needs, since it groups consecutive runs rather than sorting.
 *
 * Excluded, matching the validator:
 *
 * - an activity cannot list **itself**;
 * - an activity cannot list **the category it belongs to**, since its own time
 *   counts toward that category's total;
 * - a category cannot list **itself**;
 * - a category cannot list **any of its members**, for the same reason.
 */
export function tokenSources(config: RawConfig, subject: Subject): TokenSource[] {
  const groups = config.groups ?? [];
  const entries = config.entries ?? [];

  const asGroup = (g: { id: string; label: string }): TokenSource => ({
    value: `${GROUP_PREFIX}${g.id}`,
    label: g.label,
    kind: "group",
  });
  const asEntry = (e: { id: string; label: string }): TokenSource => ({
    value: e.id,
    label: e.label,
    kind: "entry",
  });

  if (subject.kind === "group") {
    return [
      ...groups.filter((g) => g.id !== subject.id).map(asGroup),
      ...entries.filter((e) => e.group !== subject.id).map(asEntry),
    ];
  }

  const ownGroup = entries.find((e) => e.id === subject.id)?.group;
  return [
    ...groups.filter((g) => g.id !== ownGroup).map(asGroup),
    ...entries.filter((e) => e.id !== subject.id).map(asEntry),
  ];
}
