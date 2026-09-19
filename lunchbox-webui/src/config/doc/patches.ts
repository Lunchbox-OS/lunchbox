/**
 * Typed builders for the patch vocabulary in `crates/lunchbox-config-wasm`.
 *
 * Paths address a location in the TOML document. Entries and groups are
 * addressed by `id` rather than by index, so deleting or reordering one never
 * shifts another's comments onto the wrong item.
 */

export type Json =
  | string
  | number
  | boolean
  | null
  | Json[]
  | { [key: string]: Json };

export type Patch =
  | { op: "set"; path: string; value: Json }
  | { op: "unset"; path: string }
  | { op: "insert"; path: string; index?: number; value: Json }
  | { op: "move"; path: string; from: number; to: number };

export const set = (path: string, value: Json): Patch => ({ op: "set", path, value });
export const unset = (path: string): Patch => ({ op: "unset", path });
export const insert = (path: string, value: Json, index?: number): Patch => ({
  op: "insert",
  path,
  value,
  ...(index === undefined ? {} : { index }),
});
export const move = (path: string, from: number, to: number): Patch => ({
  op: "move",
  path,
  from,
  to,
});

/**
 * The patches that take an entry's kind from `before` to `after`.
 *
 * `KindEditor` hands back the whole kind, built on the view — where serde
 * spells every unset option as `null` and fills in every default. Writing that
 * back whole plants a line for each default in the file, and before #192 failed
 * outright on the nulls. So only the fields that changed are written, and one
 * that became null is removed. A change of type replaces the kind whole, since
 * none of the old fields mean anything to the new one.
 */
export const kindPatches = (path: string, before: object, after: object): Patch[] => {
  const was = before as Record<string, Json | undefined>;
  const now = after as Record<string, Json | undefined>;
  if (was.type !== now.type) return [set(path, now as Json)];

  const patches: Patch[] = [];
  for (const key of new Set([...Object.keys(was), ...Object.keys(now)])) {
    const from = was[key] ?? null;
    const to = now[key] ?? null;
    if (JSON.stringify(from) === JSON.stringify(to)) continue;
    patches.push(to === null ? unset(`${path}.${key}`) : set(`${path}.${key}`, to));
  }
  return patches;
};

// --- path builders -------------------------------------------------------

export const entryPath = (id: string, ...rest: string[]): string =>
  ["entries[id=" + id + "]", ...rest].join(".");

export const groupPath = (id: string, ...rest: string[]): string =>
  ["groups[id=" + id + "]", ...rest].join(".");

export const servicePath = (...rest: string[]): string =>
  ["service", ...rest].join(".");

/**
 * Path to a subject's availability windows. Entries and groups carry the same
 * shape, so the schedule grid works against either.
 */
export const windowsPath = (subject: Subject): string =>
  subjectPath(subject, "availability", "windows");

export const windowPath = (subject: Subject, index: number, ...rest: string[]): string =>
  [windowsPath(subject) + `[${index}]`, ...rest].join(".");

/** An activity or a category — the two things that carry schedules and limits. */
export type Subject = { kind: "entry" | "group"; id: string };

export const subjectPath = (subject: Subject, ...rest: string[]): string =>
  subject.kind === "entry" ? entryPath(subject.id, ...rest) : groupPath(subject.id, ...rest);

/**
 * Coalesce key for a continuous gesture. Consecutive patches sharing one
 * collapse into a single undo step, which is what makes a slider drag one
 * entry in the history instead of two hundred.
 */
export const dragKey = (path: string): string => `drag:${path}`;
