// GENERATED FILE — DO NOT EDIT BY HAND
//
// Rendered from `crates/lunchbox-config-wasm/` by
// `cargo run -p lunchbox-wire-codegen --bin rpc-codegen`.
// Edit the Rust types and re-run instead.
//
// What the editor decodes back out of the wasm module: the validation report
// and the per-day availability view. The `RawConfig` projection it also
// receives is in `config.generated.ts`, rendered from the daemon's own schema.
//
// The helpers over these types — `issuesForEntry`, `MINUTES_PER_DAY` — stay
// hand-written in `report.ts` and `availability.ts`, which re-export from here.

export interface AvailabilityView {
  /**
   * What actually happens: the intersection.
   */
  effective: Span[][];
  /**
   * The activity's own windows.
   */
  entry: Span[][];
  /**
   * True when the activity places no restriction of its own (no windows, or
   * `always`), so the UI can say so rather than drawing a full week of bars.
   */
  entry_unrestricted: boolean;
  /**
   * Its group's windows, when it belongs to one.
   */
  group?: Span[][] | null;
  /**
   * Same, for the group.
   */
  group_unrestricted: boolean;
  /**
   * Windows whose `start`/`end`/`days` failed to parse. These are dropped
   * from the spans above and reported so the grid can flag them.
   */
  invalid_windows: number[];
}

/**
 * One validation error, flattened so the UI can index by activity.
 */
export interface Issue {
  /**
   * Activity this is attributable to, when the error carries one.
   */
  entry_id?: string | null;
  /**
   * Category this is attributable to, when the error carries one.
   */
  group_id?: string | null;
  /**
   * Which `ValidationError` this came from.
   */
  kind: IssueKind;
  /**
   * Human-readable text, straight from the error's `Display`.
   */
  message: string;
  /**
   * The offending literal, for errors that carry one but no id. Lets the UI
   * match a bad time or day string back to the field that holds it.
   */
  value?: string | null;
}

/**
 * Which `ValidationError` variant an issue came from.
 *
 * An enum rather than the `&'static str` this started as, so the generated
 * TypeScript is the union of these eight names instead of a bare `string`.
 */
export type IssueKind =
  /**
   * Attributable to one activity.
   */
  | "entry"
  /**
   * Attributable to one category.
   */
  | "group"
  /**
   * Two activities share an id.
   */
  | "duplicate_entry_id"
  /**
   * Two categories share an id.
   */
  | "duplicate_group_id"
  /**
   * A window's `start` or `end` is not `HH:MM`.
   */
  | "invalid_time_format"
  /**
   * A window's `days` names something that is not a day.
   */
  | "invalid_day_spec"
  /**
   * A warning fires after the session it belongs to would already have
   * ended.
   */
  | "warning_exceeds_max_run"
  /**
   * Belongs to no particular activity: service settings and the like.
   */
  | "global";

export type Report =
  /**
   * The document is not valid TOML, or does not fit the schema's shape.
   */
  | {
      kind: "syntax";
      column: number;
      line: number;
      message: string;
    }
  /**
   * Parsed, but written for a different schema version than this build knows.
   */
  | {
      kind: "version";
      expected: number;
      found: number;
    }
  /**
   * Parsed and versioned correctly. An empty `errors` list means valid.
   */
  | {
      kind: "semantic";
      errors: Issue[];
    };

/**
 * A half-open span of minutes from local midnight.
 */
export interface Span {
  end: number;
  start: number;
}

/**
 * What build of the editor this is.
 *
 * A struct rather than an ad-hoc JSON object so it has a generated TypeScript
 * mirror like everything else crossing this boundary.
 */
export interface Versions {
  /**
   * The `config_version` this build validates against. A file declaring a
   * different one comes back as a `version` report rather than being
   * edited on a schema this build does not know.
   */
  config_version: number;
  /**
   * The Lunchbox release this was built from.
   *
   * Worth showing because the standalone editor is deployed on its own
   * subdomain, decoupled from any device: a stale cached bundle is
   * indistinguishable from a current one until something disagrees with the
   * daemon, and then the first question is which build was open.
   */
  crate_version: string;
}
