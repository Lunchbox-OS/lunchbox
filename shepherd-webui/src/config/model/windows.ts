/**
 * Day masks, presets and window arithmetic for the schedule grid.
 *
 * The fiddly part of the editor, and pure, so it is unit-tested. Three rules
 * matter, all of them mirroring what the engine does
 * (`lunchbox-util/src/time.rs`):
 *
 * - Days are a bitmask, bit 0 = Monday through bit 6 = Sunday.
 * - `end` is exclusive, so `start === end` is an empty window, not all day.
 * - A window whose `start > end` wraps the clock but **not** the day: the mask
 *   is tested against the weekday of the instant, so `["fri"] 22:00-02:00`
 *   means Friday 00:00-02:00 and Friday 22:00-24:00.
 *
 * The other rule here is the editor's own: normalization must not rewrite the
 * user's spelling. `daily` stays `daily` rather than becoming `all`, and
 * `["monday"]` stays long-form, whenever the day set still matches.
 */
import type { RawDays, RawTimeWindow } from "./config.generated";
// The one `Span`, generated from `windows.rs`. This file used to declare an
// identical second one, which TypeScript's structural typing let interoperate
// with it silently — so the two could have drifted apart without a single
// error, in the one place the grid mixes spans from both sources.
import type { Span } from "./availability";

export const DAY_LABELS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"] as const;

/** Canonical short names, in bit order. */
const SHORT = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"] as const;
const LONG = [
  "monday",
  "tuesday",
  "wednesday",
  "thursday",
  "friday",
  "saturday",
  "sunday",
] as const;

export const ALL_DAYS = 0x7f;
export const WEEKDAYS = 0x1f;
export const WEEKENDS = 0x60;

/** Presets the daemon accepts, with the mask each denotes. */
const PRESETS: Record<string, number> = {
  all: ALL_DAYS,
  every: ALL_DAYS,
  daily: ALL_DAYS,
  weekdays: WEEKDAYS,
  weekends: WEEKENDS,
};

/**
 * Parse a `days` value into a bitmask, or null when it does not parse — which
 * the daemon reports as a validation error rather than guessing.
 */
export function parseDays(days: RawDays): number | null {
  if (typeof days === "string") {
    const preset = PRESETS[days.toLowerCase()];
    return preset ?? null;
  }
  let mask = 0;
  for (const day of days) {
    const key = day.toLowerCase();
    const i = SHORT.indexOf(key as (typeof SHORT)[number]);
    const j = LONG.indexOf(key as (typeof LONG)[number]);
    const bit = i >= 0 ? i : j;
    if (bit < 0) return null;
    mask |= 1 << bit;
  }
  return mask;
}

/**
 * Render a bitmask back to a `days` value, preserving how the user wrote it.
 *
 * `previous` is what the file currently says. If it still denotes the same set
 * of days, it is returned untouched — so a preset stays a preset, `daily` does
 * not become `all`, and long day names do not get abbreviated. Only when the
 * set actually changed does this pick a fresh representation.
 */
export function formatDays(mask: number, previous?: RawDays): RawDays {
  if (previous !== undefined && parseDays(previous) === mask) return previous;

  // A preset is friendlier than a seven-element list, but only pick one when
  // the user was not already using explicit days.
  const wasList = Array.isArray(previous);
  if (!wasList) {
    if (mask === ALL_DAYS) return "all";
    if (mask === WEEKDAYS) return "weekdays";
    if (mask === WEEKENDS) return "weekends";
  }

  // Match the previous list's long/short spelling when there was one.
  const useLong =
    wasList &&
    (previous as string[]).some((d) =>
      LONG.includes(d.toLowerCase() as (typeof LONG)[number]),
    );
  const names = useLong ? LONG : SHORT;
  const out: string[] = [];
  for (let bit = 0; bit < 7; bit++) {
    if (mask & (1 << bit)) out.push(names[bit]);
  }
  return out;
}

/**
 * One `HH:MM` field, exactly as `u8::from_str` reads it on the daemon side:
 * ASCII digits with an optional leading `+`, no surrounding space, and it has
 * to fit a `u8`.
 *
 * Spelling this out rather than reaching for `Number()` matters — `Number()`
 * takes `" 16"`, `"0x10"` and `"1e2"`, none of which the daemon does.
 */
function parseU8(field: string): number | null {
  if (!/^\+?[0-9]+$/.test(field)) return null;
  const n = Number(field);
  return n <= 255 ? n : null;
}

/**
 * `"16:30"` -> `990`. Null when it does not parse.
 *
 * Mirrors `parse_time` in `crates/lunchbox-config/src/validation.rs`, down to
 * the parts of it that are accidents of `u8::from_str` rather than decisions:
 * `"16:5"` and `"016:030"` are accepted because the daemon accepts them and
 * runs on them, so the grid has to draw them where they will actually take
 * effect. `crates/lunchbox-config-wasm/tests/time_formats.json` pins the pair.
 */
export function parseTime(value: string): number | null {
  const parts = value.split(":");
  if (parts.length !== 2) return null;
  const h = parseU8(parts[0]);
  const m = parseU8(parts[1]);
  if (h === null || m === null) return null;
  if (h > 23 || m > 59) return null;
  return h * 60 + m;
}

/** `990` -> `"16:30"`. Minutes past midnight; 1440 renders as `"24:00"`. */
export function formatTime(minutes: number): string {
  const clamped = Math.max(0, Math.min(MINUTES_IN_DAY, Math.round(minutes)));
  const h = Math.floor(clamped / 60);
  const m = clamped % 60;
  return `${String(h).padStart(2, "0")}:${String(m).padStart(2, "0")}`;
}

export const MINUTES_IN_DAY = 1440;

/** Snap a minute value to the grid's granularity. */
export const snap = (minutes: number, step: number): number =>
  Math.round(minutes / step) * step;

export interface ParsedWindow {
  index: number;
  mask: number;
  start: number;
  end: number;
  /** True when this window wraps midnight, so the grid draws two bands. */
  wraps: boolean;
  /** False when `days`, `start` or `end` did not parse. */
  valid: boolean;
}

export function parseWindow(w: RawTimeWindow, index: number): ParsedWindow {
  const mask = parseDays(w.days);
  const start = parseTime(w.start);
  const end = parseTime(w.end);
  const valid = mask !== null && start !== null && end !== null;
  return {
    index,
    mask: mask ?? 0,
    start: start ?? 0,
    end: end ?? 0,
    wraps: valid && (start as number) > (end as number),
    valid,
  };
}

export const parseWindows = (windows: RawTimeWindow[]): ParsedWindow[] =>
  windows.map(parseWindow);

/**
 * The bands this window occupies on one day, as half-open minute spans.
 *
 * Empty unless the day is in the mask. A wrapping window yields two bands on
 * that same day, which is what the engine evaluates — not one band running
 * into the next day.
 */
export function bandsOnDay(w: ParsedWindow, day: number): Span[] {
  if (!w.valid || !(w.mask & (1 << day))) return [];
  if (w.start === w.end) return [];
  if (w.start < w.end) return [{ start: w.start, end: w.end }];
  return [
    { start: w.start, end: MINUTES_IN_DAY },
    { start: 0, end: w.end },
  ];
}

/** Sort and coalesce overlapping or touching spans. */
export function mergeSpans(spans: Span[]): Span[] {
  if (spans.length === 0) return [];
  const sorted = [...spans].sort((a, b) => a.start - b.start || a.end - b.end);
  const out: Span[] = [{ ...sorted[0] }];
  for (const s of sorted.slice(1)) {
    const last = out[out.length - 1];
    if (s.start <= last.end) last.end = Math.max(last.end, s.end);
    else out.push({ ...s });
  }
  return out;
}

/**
 * Which windows can be merged into one after an edit: same start and end, so
 * their day sets can be unioned. Returns groups of indices, largest first, and
 * only groups with more than one member.
 */
export function mergeableGroups(windows: ParsedWindow[]): number[][] {
  const byTime = new Map<string, number[]>();
  for (const w of windows) {
    if (!w.valid) continue;
    const key = `${w.start}-${w.end}`;
    const list = byTime.get(key);
    if (list) list.push(w.index);
    else byTime.set(key, [w.index]);
  }
  return [...byTime.values()].filter((g) => g.length > 1);
}

/** A new window covering `mask` from `start` to `end`. */
export function newWindow(mask: number, start: number, end: number): RawTimeWindow {
  return {
    days: formatDays(mask),
    start: formatTime(start),
    end: formatTime(end),
  };
}

/** Whether a window is currently drawn on this day. */
export const occupiesDay = (w: ParsedWindow, day: number): boolean =>
  w.valid && (w.mask & (1 << day)) !== 0;

/** Toggle one day in a mask. */
export const toggleDay = (mask: number, day: number): number => mask ^ (1 << day);

/** How many days a mask covers. */
export const dayCount = (mask: number): number => {
  let n = 0;
  for (let bit = 0; bit < 7; bit++) if (mask & (1 << bit)) n++;
  return n;
};

/** Human summary of a mask, for a window's label. */
export function describeDays(mask: number): string {
  if (mask === ALL_DAYS) return "Every day";
  if (mask === WEEKDAYS) return "Weekdays";
  if (mask === WEEKENDS) return "Weekends";
  const names: string[] = [];
  for (let bit = 0; bit < 7; bit++) if (mask & (1 << bit)) names.push(DAY_LABELS[bit]);
  if (names.length === 0) return "No days";
  return names.join(", ");
}
