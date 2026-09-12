/**
 * What a row lets somebody do (issue #195).
 *
 * Pure, and worth its own file because two of these are easy to get backwards:
 * deleting an entry is a permission on its *parent*, and an escaping symlink
 * is listed precisely so that it can be deleted.
 */
import { describe, expect, it } from "vitest";
import {
  canDelete,
  canDownload,
  canRename,
  canWriteInto,
} from "./permissions";
import { readDrop, isFileDrag, directoriesRefused } from "./dnd";
import { nodeKey, type Row } from "./tree";
import type { DirEntryInfo, RootInfo, UnusableReason } from "./types";

function rootRow(over: Partial<RootInfo> = {}): Row {
  return {
    kind: "root",
    key: nodeKey("home", ""),
    depth: 0,
    expanded: false,
    root: {
      id: "home",
      label: "Home",
      kind: "home",
      path: "/home/kiosk",
      writable: true,
      total_bytes: 100,
      free_bytes: 50,
      ...over,
    },
  };
}

function entryRow(
  entry: Partial<DirEntryInfo>,
  parentWritable = true,
): Row {
  return {
    kind: "entry",
    key: nodeKey("home", entry.name ?? "x"),
    depth: 1,
    rootId: "home",
    path: entry.name ?? "x",
    expanded: false,
    parentWritable,
    entry: {
      name: "x",
      kind: "file",
      size: 1,
      modified: null,
      etag: "1-1",
      hidden: false,
      symlink: false,
      ...entry,
    },
  };
}

describe("who may write where", () => {
  it("takes files into a writable folder, and not into a file", () => {
    expect(canWriteInto(rootRow())).toBe(true);
    expect(canWriteInto(rootRow({ writable: false }))).toBe(false);
    expect(canWriteInto(entryRow({ kind: "dir", writable: true }))).toBe(true);
    expect(canWriteInto(entryRow({ kind: "dir", writable: false }))).toBe(false);
    expect(canWriteInto(entryRow({ kind: "file" }))).toBe(false);
  });

  it("treats a folder with no answer as no", () => {
    // The field is absent on rows that are not directories; a directory that
    // somehow arrives without it gets the answer that does not offer a
    // control which fails.
    expect(canWriteInto(entryRow({ kind: "dir", writable: undefined }))).toBe(false);
  });

  it("hangs rename and delete off the parent, not the entry", () => {
    expect(canDelete(entryRow({}, true))).toBe(true);
    expect(canDelete(entryRow({}, false))).toBe(false);
    expect(canRename(entryRow({}, false))).toBe(false);
    // A root is not something this API renames or deletes.
    expect(canDelete(rootRow())).toBe(false);
    expect(canRename(rootRow())).toBe(false);
  });

  it("still deletes a link that points out of its place", () => {
    // The entire reason such a link is listed rather than hidden.
    const escaping = entryRow({ kind: "dir", symlink: true, unusable: "symlink_escapes" });
    expect(canDelete(escaping)).toBe(true);
    expect(canRename(escaping)).toBe(false);
    expect(canWriteInto(escaping)).toBe(false);
    expect(canDownload(escaping)).toBe(false);
  });

  it("does nothing at all to a name it cannot address", () => {
    for (const reason of ["name_not_utf8", "not_browsable"] as UnusableReason[]) {
      const row = entryRow({ unusable: reason });
      expect(canDelete(row)).toBe(false);
      expect(canRename(row)).toBe(false);
      expect(canDownload(row)).toBe(false);
    }
  });

  it("downloads a file and not a folder", () => {
    expect(canDownload(entryRow({ kind: "file" }))).toBe(true);
    expect(canDownload(entryRow({ kind: "dir" }))).toBe(false);
  });
});

describe("reading a drop", () => {
  const file = new File(["x"], "a.txt");

  it("knows a file drag from anything else", () => {
    expect(isFileDrag({ types: ["Files"] })).toBe(true);
    expect(isFileDrag({ types: ["text/plain"] })).toBe(false);
    expect(isFileDrag(null)).toBe(false);
  });

  it("splits folders out, because they arrive looking like empty files", () => {
    const payload = readDrop({
      items: [
        {
          kind: "file",
          webkitGetAsEntry: () => ({ isDirectory: false, name: "a.txt" }),
          getAsFile: () => file,
        },
        {
          kind: "file",
          webkitGetAsEntry: () => ({ isDirectory: true, name: "covers" }),
          getAsFile: () => null,
        },
        // Dragged text, not a file at all.
        { kind: "string", webkitGetAsEntry: () => null, getAsFile: () => null },
      ],
      files: [file],
    });
    expect(payload.files.map((f) => f.name)).toEqual(["a.txt"]);
    expect(payload.directories).toEqual(["covers"]);
    expect(directoriesRefused(payload.directories)).toContain("covers");
  });

  it("falls back to the plain file list where there is no entry API", () => {
    expect(readDrop({ files: [file] })).toEqual({ files: [file], directories: [] });
  });
});
