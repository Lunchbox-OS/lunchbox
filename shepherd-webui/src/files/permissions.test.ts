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
import { canMove, moveRefusal, readDrop, isFileDrag, directoriesRefused } from "./dnd";
import { joinPath, nodeKey, type Row } from "./tree";
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

  it("deletes a name that is not text only when it was given a handle", () => {
    // Every other action is off -- none of them can say which file they mean.
    // Delete can, because the listing hands back the entry's own bytes; before
    // that existed, a file this device shows you and flags as broken could
    // never be got rid of from a device with no shell.
    const named = entryRow({ unusable: "name_not_utf8", handle: "636166e9" });
    expect(canDelete(named)).toBe(true);
    // And renamed, which is the repair rather than the bin: a name somebody
    // can type makes every other action work again.
    expect(canRename(named)).toBe(true);
    // Not opened or downloaded, though. Those would have to say *which* file,
    // and a handle only answers that for the two routes that take one.
    expect(canDownload(named)).toBe(false);
    expect(canWriteInto(named)).toBe(false);

    // An older device that does not send one is not offered buttons that
    // cannot work.
    const unnamed = entryRow({ unusable: "name_not_utf8" });
    expect(canDelete(unnamed)).toBe(false);
    expect(canRename(unnamed)).toBe(false);
  });
  it("still deletes something it could not explain", () => {
    // Not knowing what a thing *is* says nothing about whether its name still
    // reaches it -- a FAT drive whose charset cannot spell a stored name is
    // the usual cause, and being unable to tidy up after one would be the
    // worse answer. It is offered nothing else, because nothing else could be
    // made to work without knowing what it is.
    const puzzling = entryRow({ unusable: "unreadable", size: null, etag: null });
    expect(canDelete(puzzling)).toBe(true);
    expect(canRename(puzzling)).toBe(false);
    expect(canDownload(puzzling)).toBe(false);
    expect(canWriteInto(puzzling)).toBe(false);
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

describe("where a row may be dropped", () => {
  /** A row at `path`, in `rootId`. */
  function at(path: string, kind: "dir" | "file" = "file", rootId = "home"): Row {
    return {
      kind: "entry",
      key: nodeKey(rootId, path),
      depth: 1,
      rootId,
      path,
      expanded: false,
      parentWritable: true,
      entry: {
        name: path.split("/").pop() ?? path,
        kind,
        size: kind === "file" ? 1 : null,
        modified: null,
        etag: kind === "file" ? "1-1" : null,
        hidden: false,
        symlink: false,
        writable: kind === "dir" ? true : undefined,
      },
    };
  }

  function rootAt(id = "home"): Row {
    return {
      kind: "root",
      key: nodeKey(id, ""),
      depth: 0,
      expanded: true,
      root: {
        id,
        label: id,
        kind: "home",
        path: `/${id}`,
        writable: true,
        total_bytes: 10,
        free_bytes: 5,
      },
    };
  }

  it("moves a file into another folder", () => {
    expect(canMove(at("Downloads/hobbit.epub"), at("Books", "dir"))).toBe(true);
  });

  it("refuses the folder it is already in", () => {
    // Including the top of a place, which is the same statement about `""`.
    expect(moveRefusal(at("Books/hobbit.epub"), at("Books", "dir"))).toMatch(/already is/);
    expect(moveRefusal(at("hobbit.epub"), rootAt())).toMatch(/already is/);
  });

  it("refuses a folder into its own subtree", () => {
    // `rename("a", "a/b")` is EINVAL, and dragging a folder onto something
    // inside itself is an easy gesture to make by accident.
    expect(moveRefusal(at("Books", "dir"), at("Books/covers", "dir"))).toMatch(
      /inside itself/,
    );
    expect(moveRefusal(at("Books", "dir"), at("Books", "dir"))).toMatch(/already is/);
  });

  it("refuses a second place, because the API cannot express it", () => {
    expect(moveRefusal(at("hobbit.epub"), rootAt("usb"))).toMatch(/between places/);
  });

  it("refuses a file as a destination", () => {
    expect(moveRefusal(at("a.txt"), at("b.txt"))).toMatch(/into a folder/);
  });

  it("refuses a read-only destination, and an unmovable source", () => {
    const locked = at("Locked", "dir");
    if (locked.kind === "entry") locked.entry.writable = false;
    expect(moveRefusal(at("a.txt"), locked)).toMatch(/read-only/);

    const stuck = at("a.txt");
    if (stuck.kind === "entry") stuck.parentWritable = false;
    expect(moveRefusal(stuck, at("Books", "dir"))).toMatch(/cannot be moved/);
  });

  it("keeps the name when it lands", () => {
    // What `useFileActions.move` builds: the destination folder plus the name.
    expect(joinPath("Books", "hobbit.epub")).toBe("Books/hobbit.epub");
    expect(joinPath("", "hobbit.epub")).toBe("hobbit.epub");
  });
});
