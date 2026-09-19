/**
 * Helpers over the validation report, whose types are generated from
 * `crates/lunchbox-config-wasm/src/report.rs`.
 *
 * The three failure modes are kept apart in the Rust because the editor reacts
 * to each differently: a syntax error has a caret position and blocks
 * everything, a version mismatch means this build cannot safely edit the file
 * at all, and semantic errors are individually attributable to an activity or
 * category. Everything below is the "individually attributable" part, which is
 * what the detail panels index by.
 */
import type { Issue, Report } from "./wasm-types.generated";

export type { Issue, IssueKind, Report } from "./wasm-types.generated";

export const isValid = (r: Report | null): boolean =>
  r?.kind === "semantic" && r.errors.length === 0;

/** Every issue attributable to one activity. */
export const issuesForEntry = (r: Report | null, entryId: string): Issue[] =>
  r?.kind === "semantic" ? r.errors.filter((e) => e.entry_id === entryId) : [];

/** Every issue attributable to one category. */
export const issuesForGroup = (r: Report | null, groupId: string): Issue[] =>
  r?.kind === "semantic" ? r.errors.filter((e) => e.group_id === groupId) : [];

/** Issues that belong to no particular activity: service settings and the like. */
export const globalIssues = (r: Report | null): Issue[] =>
  r?.kind === "semantic"
    ? r.errors.filter((e) => !e.entry_id && !e.group_id)
    : [];

export const allIssues = (r: Report | null): Issue[] =>
  r?.kind === "semantic" ? r.errors : [];
