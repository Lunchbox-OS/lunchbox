/**
 * Mirrors `crates/shepherd-config-wasm/src/report.rs`.
 *
 * Three failure modes, kept apart because the editor reacts to each
 * differently: a syntax error has a caret position and blocks everything, a
 * version mismatch means this build cannot safely edit the file at all, and
 * semantic errors are individually attributable to an activity or category.
 */
export type Report =
  | { kind: "syntax"; message: string; line: number; column: number }
  | { kind: "version"; found: number; expected: number }
  | { kind: "semantic"; errors: Issue[] };

export interface Issue {
  kind:
    | "entry"
    | "group"
    | "duplicate_entry_id"
    | "duplicate_group_id"
    | "invalid_time_format"
    | "invalid_day_spec"
    | "warning_exceeds_max_run"
    | "global";
  entry_id: string | null;
  group_id: string | null;
  /**
   * The offending literal, for errors that carry one but no id — a malformed
   * `HH:MM` or an unknown day name. Lets the UI match the value back to the
   * field holding it, which is the stopgap until `ValidationError` carries
   * structured paths.
   */
  value: string | null;
  message: string;
}

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
