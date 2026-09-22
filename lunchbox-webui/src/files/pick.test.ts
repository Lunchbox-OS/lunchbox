/**
 * What a picked row becomes, and what a field's existing value points at
 * (issue #186).
 */
import { describe, expect, it } from "vitest";
import { ancestorKeys, configPath, locate, pickRefusal } from "./pick";
import type { Row } from "./tree";
import type { DirEntryInfo, RootInfo } from "./types";

const root = (over: Partial<RootInfo>): RootInfo => ({
  id: "home",
  label: "Home",
  kind: "home",
  path: "/home/kiosk",
  writable: true,
  total_bytes: null,
  free_bytes: null,
  ...over,
});

const HOME = root({});
const STICK = root({ id: "ext-0", label: "ROMS", kind: "external", path: "/media/kiosk/ROMS" });
const NAS = root({ id: "extra-0", label: "Films", kind: "configured", path: "/home/kiosk/Films" });

describe("writing a picked path", () => {
  it("spells the home root with a tilde", () => {
    expect(configPath(HOME, "Books/the-hobbit.epub")).toBe("~/Books/the-hobbit.epub");
  });

  // The home root's reported path is canonical, so an absolute one would
  // record wherever a symlinked home actually lands.
  it("spells the home root itself as ~", () => {
    expect(configPath(HOME, "")).toBe("~");
  });

  it("spells every other root absolutely", () => {
    expect(configPath(STICK, "gba/emerald.gba")).toBe("/media/kiosk/ROMS/gba/emerald.gba");
    expect(configPath(NAS, "")).toBe("/home/kiosk/Films");
  });
});

describe("finding where a field already points", () => {
  const roots = [HOME, STICK, NAS];

  it("reads a tilde path against the home root", () => {
    expect(locate("~/Books/the-hobbit.epub", roots)).toEqual({
      rootId: "home",
      path: "Books/the-hobbit.epub",
    });
    expect(locate("~", roots)).toEqual({ rootId: "home", path: "" });
  });

  it("reads an absolute path against whichever root holds it", () => {
    expect(locate("/media/kiosk/ROMS/gba/emerald.gba", roots)).toEqual({
      rootId: "ext-0",
      path: "gba/emerald.gba",
    });
  });

  // Otherwise a configured root inside the home would open as a long path in
  // the home, which is the same file and the wrong place to be shown.
  it("prefers the most specific root when they nest", () => {
    expect(locate("/home/kiosk/Films/movies.toml", roots)).toEqual({
      rootId: "extra-0",
      path: "movies.toml",
    });
  });

  it("gives up on anything that names nothing here", () => {
    expect(locate("", roots)).toBeNull();
    expect(locate("https://youtube.com/playlist?list=PL1", roots)).toBeNull();
    expect(locate("applications-games", roots)).toBeNull();
    expect(locate("Books/the-hobbit.epub", roots)).toBeNull();
    expect(locate("/srv/elsewhere/film.mkv", roots)).toBeNull();
  });

  it("finds nothing in a tilde path when the device offers no home", () => {
    expect(locate("~/Books", [STICK])).toBeNull();
  });
});

describe("the folders a location needs open", () => {
  it("lists the root and every directory above it, but not itself", () => {
    expect(ancestorKeys({ rootId: "home", path: "Books/sf/dune.epub" })).toEqual([
      "home\u0000",
      "home\u0000Books",
      "home\u0000Books/sf",
    ]);
  });

  it("is just the root for something at the top", () => {
    expect(ancestorKeys({ rootId: "home", path: "dune.epub" })).toEqual(["home\u0000"]);
  });
});

const entry = (over: Partial<DirEntryInfo>): Row => ({
  kind: "entry",
  key: "home\u0000x",
  depth: 1,
  rootId: "home",
  path: "x",
  expanded: false,
  parentWritable: true,
  entry: {
    name: "x",
    kind: "file",
    size: 1,
    modified: null,
    etag: null,
    hidden: false,
    symlink: false,
    ...over,
  },
});

describe("what may be picked", () => {
  it("takes a file for a file and a folder for a folder", () => {
    expect(pickRefusal(entry({ kind: "file" }), "file")).toBeNull();
    expect(pickRefusal(entry({ kind: "dir" }), "directory")).toBeNull();
  });

  it("says which way round it got them", () => {
    expect(pickRefusal(entry({ kind: "dir" }), "file")).toBe("That is a folder.");
    expect(pickRefusal(entry({ kind: "file" }), "directory")).toBe(
      "That is a file, not a folder.",
    );
  });

  // RetroArch content: a few cores load a directory rather than a file.
  it("takes either where either will launch", () => {
    expect(pickRefusal(entry({ kind: "file" }), "either")).toBeNull();
    expect(pickRefusal(entry({ kind: "dir" }), "either")).toBeNull();
  });

  // TOML is UTF-8. This file can be listed and renamed, and never named by a
  // policy as it stands.
  it("refuses a name that is not text, whatever is being asked for", () => {
    const broken = entry({ unusable: "name_not_utf8", handle: "op-3" });
    for (const kind of ["file", "directory", "either"] as const) {
      expect(pickRefusal(broken, kind)).toBe(
        "That name is not text, so no config can refer to it.",
      );
    }
  });

  it("refuses what the device itself will not open", () => {
    expect(pickRefusal(entry({ unusable: "symlink_escapes" }), "file")).toBe(
      "This device will not open that one.",
    );
  });

  it("takes a whole place where a folder is wanted, but never as a file", () => {
    const place: Row = { kind: "root", key: "home\u0000", depth: 0, root: HOME, expanded: true };
    expect(pickRefusal(place, "directory")).toBeNull();
    expect(pickRefusal(place, "either")).toBeNull();
    expect(pickRefusal(place, "file")).toBe("That is a place, not a file.");
  });

  it("has nothing to say about no selection at all", () => {
    expect(pickRefusal(null, "file")).toBeNull();
  });
});
