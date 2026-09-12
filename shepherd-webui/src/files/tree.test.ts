/**
 * The tree's arithmetic (issue #195).
 *
 * No DOM here on purpose: the walk, the sort and the pruning are where the
 * awkward cases live, and they are all pure.
 */
import { describe, expect, it } from "vitest";
import {
  buildRows,
  isWithin,
  nodeKey,
  reachableExpanded,
  sortEntries,
  splitKey,
  type DirectoryState,
  type NodeKey,
  type SortSpec,
} from "./tree";
import { fileTreeReducer, initialTreeState } from "./useFileTree";
import type { DirEntryInfo, Listing, RootInfo } from "./types";

const BY_NAME: SortSpec = { column: "name", direction: "asc" };

function root(id: string, label = id): RootInfo {
  return {
    id,
    label,
    kind: id === "home" ? "home" : "external",
    path: `/${id}`,
    writable: true,
    total_bytes: 1000,
    free_bytes: 500,
  };
}

function entry(name: string, over: Partial<DirEntryInfo> = {}): DirEntryInfo {
  return {
    name,
    kind: "file",
    size: 10,
    modified: "2026-09-01T12:00:00-04:00",
    etag: `10-1`,
    hidden: name.startsWith("."),
    symlink: false,
    ...over,
  };
}

function dir(name: string, over: Partial<DirEntryInfo> = {}): DirEntryInfo {
  return entry(name, { kind: "dir", size: null, etag: null, ...over });
}

function listing(
  rootId: string,
  path: string,
  entries: DirEntryInfo[],
  over: Partial<Listing> = {},
): [NodeKey, DirectoryState] {
  return [
    nodeKey(rootId, path),
    {
      status: "success",
      listing: {
        root: rootId,
        path,
        writable: true,
        entries,
        truncated: false,
        cursor: null,
        ...over,
      },
    },
  ];
}

describe("node keys", () => {
  it("survive a round trip, including an empty path", () => {
    expect(splitKey(nodeKey("home", ""))).toEqual({ rootId: "home", path: "" });
    expect(splitKey(nodeKey("ext-9f2a", "Books/covers"))).toEqual({
      rootId: "ext-9f2a",
      path: "Books/covers",
    });
  });

  it("know what is inside what", () => {
    expect(isWithin(nodeKey("home", "Books/covers"), nodeKey("home", "Books"))).toBe(
      true,
    );
    expect(isWithin(nodeKey("home", "Books"), nodeKey("home", "Books"))).toBe(true);
    // A name that merely starts the same is not inside it.
    expect(isWithin(nodeKey("home", "Bookshelf"), nodeKey("home", "Books"))).toBe(
      false,
    );
    // Everything is inside a root, and nothing crosses roots.
    expect(isWithin(nodeKey("home", "Books"), nodeKey("home", ""))).toBe(true);
    expect(isWithin(nodeKey("usb", "Books"), nodeKey("home", ""))).toBe(false);
  });
});

describe("which folders get fetched", () => {
  it("skips one whose parent is shut, without forgetting it", () => {
    const expanded = new Set([
      nodeKey("home", ""),
      nodeKey("home", "Books"),
      nodeKey("home", "Books/covers"),
    ]);
    expect(reachableExpanded(expanded, [root("home")]).sort()).toEqual(
      [...expanded].sort(),
    );

    // Close `Books`. Its child stays remembered — reopening should put it back
    // the way it was — but neither is asked for.
    expanded.delete(nodeKey("home", "Books"));
    expect(reachableExpanded(expanded, [root("home")])).toEqual([
      nodeKey("home", ""),
    ]);
  });

  it("drops a root that is no longer connected", () => {
    const expanded = new Set([nodeKey("usb", ""), nodeKey("usb", "Photos")]);
    expect(reachableExpanded(expanded, [root("home")])).toEqual([]);
  });
});

describe("sorting", () => {
  const entries = [
    entry("zeta.txt", { size: 5 }),
    dir("Alpha"),
    entry("beta.txt", { size: 500 }),
    entry(".hidden"),
    dir("omega"),
  ];

  it("puts folders first and sorts case-insensitively", () => {
    const sorted = sortEntries(entries, BY_NAME, true, false, true);
    expect(sorted.map((e) => e.name)).toEqual([
      "Alpha",
      "omega",
      "beta.txt",
      "zeta.txt",
    ]);
  });

  it("keeps folders on top when the direction flips", () => {
    const sorted = sortEntries(
      entries,
      { column: "name", direction: "desc" },
      true,
      false,
      true,
    );
    expect(sorted.map((e) => e.name)).toEqual([
      "omega",
      "Alpha",
      "zeta.txt",
      "beta.txt",
    ]);
  });

  it("interleaves when folders-first is off", () => {
    const sorted = sortEntries(entries, BY_NAME, false, false, true);
    expect(sorted.map((e) => e.name)).toEqual([
      "Alpha",
      "beta.txt",
      "omega",
      "zeta.txt",
    ]);
  });

  it("sorts numbers the way a ROM folder needs", () => {
    const discs = [entry("disc10.chd"), entry("disc2.chd"), entry("disc1.chd")];
    expect(
      sortEntries(discs, BY_NAME, true, false, true).map((e) => e.name),
    ).toEqual(["disc1.chd", "disc2.chd", "disc10.chd"]);
  });

  it("shows hidden entries only when asked", () => {
    expect(
      sortEntries(entries, BY_NAME, true, true, true).map((e) => e.name),
    ).toContain(".hidden");
    expect(
      sortEntries(entries, BY_NAME, true, false, true).map((e) => e.name),
    ).not.toContain(".hidden");
  });

  it("leaves a half-loaded folder in the server's order", () => {
    // The API's cursor is a position in *its* order, so re-sorting a folder
    // that stopped short would show a page boundary that makes no sense.
    const sorted = sortEntries(
      entries,
      { column: "size", direction: "desc" },
      true,
      false,
      false,
    );
    expect(sorted.map((e) => e.name)).toEqual([
      "zeta.txt",
      "Alpha",
      "beta.txt",
      "omega",
    ]);
  });
});

describe("the walk", () => {
  const roots = [root("home"), root("usb", "KINGSTON")];

  it("lists the roots and nothing else while everything is shut", () => {
    const rows = buildRows({
      roots,
      expanded: new Set(),
      directories: new Map(),
      sort: BY_NAME,
      foldersFirst: true,
      showHidden: false,
    });
    expect(rows).toHaveLength(2);
    expect(rows.every((r) => r.kind === "root" && r.depth === 0)).toBe(true);
  });

  it("nests children under the folder they came from", () => {
    const rows = buildRows({
      roots: [root("home")],
      expanded: new Set([nodeKey("home", ""), nodeKey("home", "Books")]),
      directories: new Map([
        listing("home", "", [dir("Books"), entry("notes.txt")]),
        listing("home", "Books", [entry("hobbit.epub")]),
      ]),
      sort: BY_NAME,
      foldersFirst: true,
      showHidden: false,
    });
    expect(
      rows.map((r) => [
        r.kind === "root" ? r.root.label : r.kind === "entry" ? r.entry.name : r.kind,
        r.depth,
      ]),
    ).toEqual([
      ["home", 0],
      ["Books", 1],
      ["hobbit.epub", 2],
      ["notes.txt", 1],
    ]);
  });

  it("holds a place for a folder that has not answered yet", () => {
    const rows = buildRows({
      roots: [root("home")],
      expanded: new Set([nodeKey("home", "")]),
      directories: new Map(),
      sort: BY_NAME,
      foldersFirst: true,
      showHidden: false,
    });
    expect(rows.map((r) => r.kind)).toEqual(["root", "pending"]);
    // At the depth the children will have, so nothing jumps when they arrive.
    expect(rows[1].depth).toBe(1);
  });

  it("keeps a folder open when its listing failed", () => {
    const rows = buildRows({
      roots: [root("home")],
      expanded: new Set([nodeKey("home", "")]),
      directories: new Map<NodeKey, DirectoryState>([
        [nodeKey("home", ""), { status: "error", error: "Nope." }],
      ]),
      sort: BY_NAME,
      foldersFirst: true,
      showHidden: false,
    });
    expect(rows[1]).toMatchObject({ kind: "error", message: "Nope." });
  });

  it("says so when a folder stopped short", () => {
    const rows = buildRows({
      roots: [root("home")],
      expanded: new Set([nodeKey("home", "")]),
      directories: new Map([
        listing("home", "", [entry("a.txt")], { truncated: true, cursor: "x" }),
      ]),
      sort: BY_NAME,
      foldersFirst: true,
      showHidden: false,
    });
    expect(rows.map((r) => r.kind)).toEqual(["root", "entry", "more"]);
  });

  it("will not expand a link that points out of its root", () => {
    const escaping = dir("escape", { symlink: true, unusable: "symlink_escapes" });
    const rows = buildRows({
      roots: [root("home")],
      expanded: new Set([nodeKey("home", ""), nodeKey("home", "escape")]),
      directories: new Map([listing("home", "", [escaping])]),
      sort: BY_NAME,
      foldersFirst: true,
      showHidden: false,
    });
    // Listed — a person has to be able to see it to delete it — and shut.
    expect(rows).toHaveLength(2);
    expect(rows[1]).toMatchObject({ kind: "entry", expanded: false });
  });
});

describe("the reducer", () => {
  it("remembers a subtree through a collapse", () => {
    let state = initialTreeState;
    for (const key of [nodeKey("home", ""), nodeKey("home", "Books")]) {
      state = fileTreeReducer(state, { type: "expand", key });
    }
    state = fileTreeReducer(state, { type: "collapse", key: nodeKey("home", "") });
    expect(state.expanded.has(nodeKey("home", "Books"))).toBe(true);
  });

  it("forgets one after a delete", () => {
    // Without this, a folder created later with the same name arrives
    // mysteriously pre-expanded, with a stale subtree under it.
    let state = initialTreeState;
    for (const key of [
      nodeKey("home", ""),
      nodeKey("home", "Books"),
      nodeKey("home", "Books/covers"),
      nodeKey("home", "Games"),
    ]) {
      state = fileTreeReducer(state, { type: "expand", key });
    }
    state = fileTreeReducer(state, {
      type: "select",
      key: nodeKey("home", "Books/covers"),
    });
    state = fileTreeReducer(state, {
      type: "forgetSubtree",
      key: nodeKey("home", "Books"),
    });
    expect([...state.expanded].sort()).toEqual(
      [nodeKey("home", ""), nodeKey("home", "Games")].sort(),
    );
    // And the caret does not stay on something that is gone.
    expect(state.selected).toBeNull();
  });

  it("turns a column around when it is clicked twice", () => {
    let state = fileTreeReducer(initialTreeState, { type: "sort", column: "size" });
    expect(state.sort).toEqual({ column: "size", direction: "asc" });
    state = fileTreeReducer(state, { type: "sort", column: "size" });
    expect(state.sort).toEqual({ column: "size", direction: "desc" });
    // A different column starts ascending rather than inheriting.
    state = fileTreeReducer(state, { type: "sort", column: "name" });
    expect(state.sort).toEqual({ column: "name", direction: "asc" });
  });
});
