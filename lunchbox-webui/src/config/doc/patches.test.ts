/**
 * The TypeScript half of the patch wire contract.
 *
 * These builders produce the JSON that `crates/lunchbox-config-wasm` parses,
 * and nothing else checks that the two agree: the Rust tests construct `Patch`
 * values directly and never see this code, and this code never leaves the
 * browser. A renamed serde tag, a reordered tuple, a path grammar that drifted
 * — any of them would leave both suites green while the editor quietly stopped
 * being able to edit anything.
 *
 * So both sides assert against one shared fixture. This file checks the
 * builders produce exactly those objects; `tests/patch_contract.rs` checks each
 * one deserializes and does what it claims.
 */
import { describe, expect, it } from "vitest";
import shapes from "../../../../crates/lunchbox-config-wasm/tests/patch_shapes.json";
import {
  entryPath,
  groupPath,
  insert,
  kindPatches,
  move,
  servicePath,
  set,
  subjectPath,
  unset,
  windowPath,
  windowsPath,
  type Patch,
} from "./patches";

/** What each named shape in the fixture should be, built the way the app builds it. */
const BUILT: Record<string, Patch> = {
  set_integer: set(servicePath("default_max_run_seconds"), 1800),
  set_string: set(entryPath("a", "label"), "Renamed"),
  set_boolean: set(entryPath("a", "disabled"), true),
  set_float: set(entryPath("a", "tokens", "earn_ratio"), 0.5),
  set_array: set(entryPath("a", "requires_input"), ["keyboard", "mouse"]),
  set_object: set(entryPath("a", "kind"), { type: "flatpak", app_id: "org.kde.krita" }),
  set_nested_index: set(windowPath({ kind: "entry", id: "a" }, 0, "start"), "07:30"),
  unset_key: unset(entryPath("a", "icon")),
  unset_array_element: unset(`${windowsPath({ kind: "entry", id: "a" })}[0]`),
  unset_by_id: unset(entryPath("a")),
  insert_append: insert("entries", {
    id: "added",
    label: "Added",
    kind: { type: "process", command: "/bin/true" },
  }),
  insert_at_index: insert(
    windowsPath({ kind: "entry", id: "a" }),
    { days: "weekends", start: "10:00", end: "12:00" },
    0,
  ),
  move_within_array: move(entryPath("a", "warnings"), 0, 1),
};

const fixture = Object.fromEntries(
  Object.entries(shapes as Record<string, unknown>).filter(([k]) => !k.startsWith("_")),
);

describe("the patch wire contract", () => {
  it("covers every shape the fixture pins", () => {
    // A shape added for the Rust side and never built here would be a gap in
    // the contract, not a passing test.
    expect(Object.keys(BUILT).sort()).toEqual(Object.keys(fixture).sort());
  });

  it("builds exactly the shapes the Rust side parses", () => {
    expect(BUILT).toEqual(fixture);
  });

  it("omits `index` when appending, rather than sending null", () => {
    // serde would reject a null where it expects Option<usize> written as an
    // absent key, so this is load-bearing rather than cosmetic.
    expect("index" in insert("entries", { id: "x" })).toBe(false);
    expect(insert("entries", { id: "x" }, 2)).toHaveProperty("index", 2);
  });
});

describe("the path grammar", () => {
  it("addresses entries and groups by id, not by index", () => {
    // Id-addressing is what stops a deletion shifting another item's comments.
    expect(entryPath("minecraft")).toBe("entries[id=minecraft]");
    expect(entryPath("minecraft", "limits", "max_run_seconds")).toBe(
      "entries[id=minecraft].limits.max_run_seconds",
    );
    expect(groupPath("games", "tokens", "from")).toBe("groups[id=games].tokens.from");
  });

  it("routes a subject to the right collection", () => {
    expect(subjectPath({ kind: "entry", id: "a" }, "limits")).toBe("entries[id=a].limits");
    expect(subjectPath({ kind: "group", id: "a" }, "limits")).toBe("groups[id=a].limits");
  });

  it("indexes into availability windows", () => {
    const subject = { kind: "group", id: "games" } as const;
    expect(windowsPath(subject)).toBe("groups[id=games].availability.windows");
    expect(windowPath(subject, 2, "end")).toBe(
      "groups[id=games].availability.windows[2].end",
    );
  });

  it("puts service settings under [service]", () => {
    expect(servicePath("volume", "max_volume")).toBe("service.volume.max_volume");
  });
});

describe("writing a kind back (issue #192)", () => {
  const path = entryPath("the-hobbit", "kind");
  // As the view has it: every default filled in, every unset option null.
  const hobbit = {
    type: "ebook",
    book: "~/Books/the-hobbit.epub",
    viewer: "okular",
    open_at: null,
    layout: "facing_first_centered",
    font_size: 16,
    font_family: "Noto Serif",
    command: null,
    args: [],
    env: {},
    kiosk: true,
  };

  it("writes only the field that changed", () => {
    expect(kindPatches(path, hobbit, { ...hobbit, book: "~/Books/b.epub" })).toEqual([
      set(`${path}.book`, "~/Books/b.epub"),
    ]);
  });

  it("removes a field that became null rather than writing null", () => {
    expect(kindPatches(path, { ...hobbit, open_at: 12 }, hobbit)).toEqual([
      unset(`${path}.open_at`),
    ]);
  });

  it("writes nothing when nothing changed", () => {
    expect(kindPatches(path, hobbit, { ...hobbit, env: {} })).toEqual([]);
  });

  it("replaces the whole kind when its type changes", () => {
    const flatpak = { type: "flatpak", app_id: "org.kde.krita" };
    expect(kindPatches(path, hobbit, flatpak)).toEqual([set(path, flatpak)]);
  });
});
