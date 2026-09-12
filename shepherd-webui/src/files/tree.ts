/**
 * The tree, as arithmetic (issue #195).
 *
 * Everything here is pure: keys, the sort, and the walk that turns "these
 * roots, these expanded keys, these listings" into the flat list of rows the
 * table renders. It is the half of the file manager that can be tested without
 * a browser, and it is where the awkward cases live — pruning a subtree after
 * a delete, a folder that loaded only part of itself, a link that points out
 * of its root.
 *
 * The flat list is also what keeps windowing a later wrapper rather than a
 * rewrite: rows are an array, and no row reaches into the tree.
 */
import type { DirEntryInfo, Listing, RootInfo } from "./types";

/**
 * A node's identity: the root id, a NUL, and the path.
 *
 * NUL separates because it cannot occur in either half — a root id is
 * `[a-z0-9-]` and the API refuses any path component containing one — so the
 * key can be split back apart without an escape scheme.
 */
export type NodeKey = string;

/** The separator, and the prefix that marks a row which is not a node. */
const SEP = "\u0000";
const FILLER = "\u0001";

export function nodeKey(rootId: string, path: string): NodeKey {
  return `${rootId}${SEP}${path}`;
}

export function splitKey(key: NodeKey): { rootId: string; path: string } {
  const at = key.indexOf(SEP);
  return { rootId: key.slice(0, at), path: key.slice(at + 1) };
}

/** `"Books/covers"` to `"Books"`; a top-level path to `""`. */
export function parentPath(path: string): string {
  const at = path.lastIndexOf("/");
  return at < 0 ? "" : path.slice(0, at);
}

export function joinPath(dir: string, name: string): string {
  return dir === "" ? name : `${dir}/${name}`;
}

/** Whether `key` is `ancestor` or lies beneath it. */
export function isWithin(key: NodeKey, ancestor: NodeKey): boolean {
  if (key === ancestor) return true;
  const a = splitKey(ancestor);
  const k = splitKey(key);
  if (a.rootId !== k.rootId) return false;
  if (a.path === "") return true;
  return k.path.startsWith(`${a.path}/`);
}

export type SortColumn = "name" | "size" | "kind" | "modified";
export type SortDirection = "asc" | "desc";

export interface SortSpec {
  column: SortColumn;
  direction: SortDirection;
}

/** What a directory's query is doing, as the walk needs to see it. */
export interface DirectoryState {
  status: "pending" | "error" | "success";
  listing?: Listing;
  error?: string;
}

export type Row =
  | {
      kind: "root";
      key: NodeKey;
      depth: 0;
      root: RootInfo;
      expanded: boolean;
    }
  | {
      kind: "entry";
      key: NodeKey;
      depth: number;
      rootId: string;
      /** Path of the entry itself, relative to its root. */
      path: string;
      entry: DirEntryInfo;
      expanded: boolean;
      /** Whether the containing directory can be written to. */
      parentWritable: boolean;
    }
  | { kind: "pending"; key: NodeKey; depth: number }
  | { kind: "error"; key: NodeKey; depth: number; node: NodeKey; message: string }
  | { kind: "empty"; key: NodeKey; depth: number }
  | { kind: "more"; key: NodeKey; depth: number; node: NodeKey; shown: number };

export interface WalkOptions {
  roots: RootInfo[];
  expanded: ReadonlySet<NodeKey>;
  directories: ReadonlyMap<NodeKey, DirectoryState>;
  sort: SortSpec;
  foldersFirst: boolean;
  showHidden: boolean;
}

/**
 * Which expanded keys are actually on screen.
 *
 * A node whose parent is collapsed stays in `expanded` — so reopening the
 * parent puts it back the way it was — but must not be fetched. Reachability
 * is pure string arithmetic: every ancestor path must also be expanded, and
 * the root itself must be.
 */
export function reachableExpanded(
  expanded: ReadonlySet<NodeKey>,
  roots: RootInfo[],
): NodeKey[] {
  const rootIds = new Set(roots.map((r) => r.id));
  return [...expanded].filter((key) => {
    const { rootId, path } = splitKey(key);
    if (!rootIds.has(rootId)) return false;
    if (path === "") return true;
    if (!expanded.has(nodeKey(rootId, ""))) return false;
    const parts = path.split("/");
    for (let i = 1; i < parts.length; i += 1) {
      if (!expanded.has(nodeKey(rootId, parts.slice(0, i).join("/")))) return false;
    }
    return true;
  });
}

/**
 * Order a directory's entries.
 *
 * `canSort` is false for a directory that stopped short of loading everything:
 * the API's cursor is a position in *its* order, so sorting half a folder by
 * size would show a page boundary that makes no sense. Those folders keep the
 * server's order — folders first, then case-insensitive name — and the table
 * says so.
 */
export function sortEntries(
  entries: DirEntryInfo[],
  sort: SortSpec,
  foldersFirst: boolean,
  showHidden: boolean,
  canSort: boolean,
): DirEntryInfo[] {
  const visible = showHidden ? [...entries] : entries.filter((e) => !e.hidden);
  if (!canSort) return visible;
  const sign = sort.direction === "asc" ? 1 : -1;
  return visible.sort((a, b) => {
    if (foldersFirst) {
      const rank = rankOf(a) - rankOf(b);
      // Not multiplied by `sign`: reversing the sort should turn the names
      // around, not float the files above the folders.
      if (rank !== 0) return rank;
    }
    return sign * compare(a, b, sort.column);
  });
}

function rankOf(entry: DirEntryInfo): number {
  return entry.kind === "dir" ? 0 : 1;
}

function compare(a: DirEntryInfo, b: DirEntryInfo, column: SortColumn): number {
  switch (column) {
    case "size":
      return (a.size ?? -1) - (b.size ?? -1) || byName(a, b);
    case "modified":
      return (
        Date.parse(a.modified ?? "") - Date.parse(b.modified ?? "") || byName(a, b)
      );
    case "kind":
      return extensionOf(a.name).localeCompare(extensionOf(b.name)) || byName(a, b);
    case "name":
    default:
      return byName(a, b);
  }
}

function byName(a: DirEntryInfo, b: DirEntryInfo): number {
  // `numeric` so `disc2` sorts before `disc10`, which is the whole difference
  // between a usable ROM folder and an annoying one.
  return a.name.localeCompare(b.name, undefined, {
    sensitivity: "base",
    numeric: true,
  });
}

export function extensionOf(name: string): string {
  const at = name.lastIndexOf(".");
  return at > 0 ? name.slice(at + 1).toLowerCase() : "";
}

/** Whether a row can be expanded at all. */
export function isExpandable(entry: DirEntryInfo): boolean {
  return entry.kind === "dir" && entry.unusable === undefined;
}

/**
 * The whole visible tree, depth-first, as a flat array.
 */
export function buildRows(options: WalkOptions): Row[] {
  const { roots, expanded, directories } = options;
  const rows: Row[] = [];

  for (const root of roots) {
    const key = nodeKey(root.id, "");
    const isExpanded = expanded.has(key);
    rows.push({ kind: "root", key, depth: 0, root, expanded: isExpanded });
    if (isExpanded) {
      pushDirectory(rows, root.id, "", 1);
    }
  }
  return rows;

  function pushDirectory(
    out: Row[],
    rootId: string,
    path: string,
    depth: number,
  ): void {
    const key = nodeKey(rootId, path);
    const state = directories.get(key);
    if (!state || state.status === "pending") {
      out.push({ kind: "pending", key: `${key}${FILLER}pending`, depth });
      return;
    }
    if (state.status === "error" || !state.listing) {
      out.push({
        kind: "error",
        key: `${key}${FILLER}error`,
        depth,
        node: key,
        message: state.error ?? "That folder could not be read.",
      });
      return;
    }
    const listing = state.listing;
    const entries = sortEntries(
      listing.entries,
      options.sort,
      options.foldersFirst,
      options.showHidden,
      !listing.truncated,
    );
    if (entries.length === 0) {
      out.push({ kind: "empty", key: `${key}${FILLER}empty`, depth });
      return;
    }
    for (const entry of entries) {
      const childPath = joinPath(path, entry.name);
      const childKey = nodeKey(rootId, childPath);
      const childExpanded = isExpandable(entry) && expanded.has(childKey);
      out.push({
        kind: "entry",
        key: childKey,
        depth,
        rootId,
        path: childPath,
        entry,
        expanded: childExpanded,
        parentWritable: listing.writable,
      });
      if (childExpanded) {
        pushDirectory(out, rootId, childPath, depth + 1);
      }
    }
    if (listing.truncated) {
      out.push({
        kind: "more",
        key: `${key}${FILLER}more`,
        depth,
        node: key,
        shown: entries.length,
      });
    }
  }
}

/** A row that stands for a node, rather than for a directory's state. */
export type NodeRow = Extract<Row, { kind: "root" } | { kind: "entry" }>;

/**
 * Rows a caret can land on. The filler rows are not selectable.
 *
 * A type guard rather than a predicate, so that the keyboard handler can ask
 * "is this expandable" of the row it just found without re-checking the kind.
 */
export function isSelectable(row: Row): row is NodeRow {
  return row.kind === "root" || row.kind === "entry";
}
