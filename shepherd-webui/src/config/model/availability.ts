/**
 * Mirrors `crates/shepherd-config-wasm/src/windows.rs`.
 *
 * Spans are half-open minute ranges from local midnight, seven days Monday
 * first, matching the day bitmask's bit order.
 */
export interface Span {
  start: number;
  end: number;
}

/** Seven days of spans, Monday first. */
export type Week = Span[][];

export interface AvailabilityView {
  /** The subject's own windows. */
  entry: Week;
  /** Its group's windows, when it belongs to one. */
  group: Week | null;
  /** What the engine actually enforces: the intersection. */
  effective: Week;
  /**
   * True when the subject places no restriction of its own — no windows, or
   * `always = true`. The engine treats an empty window list as always
   * available, so the grid must render this as a full week rather than an
   * empty one.
   */
  entry_unrestricted: boolean;
  group_unrestricted: boolean;
  /** Indices of windows whose `days`/`start`/`end` failed to parse. */
  invalid_windows: number[];
}

export const MINUTES_PER_DAY = 1440;
