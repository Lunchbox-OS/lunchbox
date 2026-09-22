/**
 * Turning a row of the file tree into a path a config can hold, and back
 * (issue #186).
 *
 * Pure, and the place a `~/` bug would otherwise hide.
 *
 * ## Why the home root is written as `~/`
 *
 * The API speaks `(root id, path relative to that root)` and never accepts an
 * absolute path back; a policy wants an absolute path or one starting `~/`.
 * For the home root the two are not merely interchangeable — `~/` is the
 * better answer, and exactly right:
 *
 * - The home root *is* `$HOME` of the process that will do the launching.
 *   lunchboxd builds the root from `lunchbox_util::home_dir()` and expands a
 *   `~/` at launch with `dirs::home_dir()`, which is the same variable read in
 *   the same process.
 * - A root's reported `path` has been through `canonicalize`, so a home
 *   reached through a symlink would otherwise be written as its target — a
 *   path that works today and says something nobody wrote.
 *
 * Every other root is written absolute, because that is the only spelling
 * there is. For a removable drive that means a path under `/media`, which is
 * where udisks put it *this boot* — the one case where a picked path can stop
 * being true on its own, and why `content` on a USB stick is worth a second
 * thought.
 */
import { nodeKey, type NodeKey, type Row } from "./tree";
import type { RootInfo } from "./types";
import type { PathKind } from "../config/pick/FilePicker";

/** A place in the tree, in the terms every file route speaks. */
export interface Location {
  rootId: string;
  /** Relative to the root; `""` is the root itself. */
  path: string;
}

/** What to write into the config for a path inside `root`. */
export function configPath(root: RootInfo, path: string): string {
  if (root.kind === "home") return path === "" ? "~" : `~/${path}`;
  const base = root.path.endsWith("/") ? root.path.slice(0, -1) : root.path;
  return path === "" ? base || "/" : `${base}/${path}`;
}

/** The same, for whatever the tree has selected. */
export function configPathOf(row: Row, roots: RootInfo[]): string | null {
  if (row.kind === "root") return configPath(row.root, "");
  if (row.kind !== "entry") return null;
  const root = roots.find((r) => r.id === row.rootId);
  return root ? configPath(root, row.path) : null;
}

/**
 * Where a path the field already holds lives, so the dialog can open there.
 *
 * Null for everything that names nothing on this device, which is an ordinary
 * answer rather than a failure: an empty field, a YouTube playlist URL, a
 * theme icon name, a relative path, a file on a drive that is not plugged in,
 * or a path under a directory this device does not offer. The dialog then
 * opens at the roots.
 */
export function locate(value: string, roots: RootInfo[]): Location | null {
  const home = roots.find((r) => r.kind === "home");
  if (value === "~" && home) return { rootId: home.id, path: "" };
  if (value.startsWith("~/") && home) {
    return { rootId: home.id, path: trimSlashes(value.slice(2)) };
  }
  if (!value.startsWith("/")) return null;

  // Longest match, so a configured root nested inside the home opens as
  // itself rather than as a long path inside the home.
  let best: Location | null = null;
  let bestLength = -1;
  for (const root of roots) {
    const base = root.path.endsWith("/") ? root.path.slice(0, -1) : root.path;
    if (value !== base && !value.startsWith(`${base}/`)) continue;
    if (base.length <= bestLength) continue;
    bestLength = base.length;
    best = { rootId: root.id, path: trimSlashes(value.slice(base.length)) };
  }
  return best;
}

/**
 * The nodes that have to be open for `location` to be on screen: its root,
 * then every directory above it. Not the location itself — a file cannot be
 * expanded, and a directory the person is being shown does not need to be.
 */
export function ancestorKeys(location: Location): NodeKey[] {
  const keys = [nodeKey(location.rootId, "")];
  const parts = location.path === "" ? [] : location.path.split("/");
  for (let i = 1; i < parts.length; i += 1) {
    keys.push(nodeKey(location.rootId, parts.slice(0, i).join("/")));
  }
  return keys;
}

/**
 * Why the selected row cannot be the answer, or null if it can.
 *
 * Phrased for a person and shown beside the confirm button, rather than
 * making the row inert: "that one is a folder" is a thing somebody can act on,
 * and a row that quietly refuses clicks is not.
 */
export function pickRefusal(row: Row | null, kind: PathKind): string | null {
  if (!row) return null;
  if (row.kind === "root") {
    return kind === "file" ? "That is a place, not a file." : null;
  }
  if (row.kind !== "entry") return null;
  if (row.entry.unusable === "name_not_utf8") {
    // TOML is UTF-8, so no policy can name this file at all. Renaming it on
    // the Files tab is the repair, which is what the handle is for.
    return "That name is not text, so no config can refer to it.";
  }
  if (row.entry.unusable !== undefined) {
    return "This device will not open that one.";
  }
  const isDir = row.entry.kind === "dir";
  if (kind === "file" && isDir) return "That is a folder.";
  if (kind === "directory" && !isDir) return "That is a file, not a folder.";
  if (row.entry.kind === "other") return "That is not a file or a folder.";
  return null;
}

function trimSlashes(path: string): string {
  return path.replace(/^\/+/, "").replace(/\/+$/, "");
}
