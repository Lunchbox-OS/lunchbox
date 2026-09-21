/**
 * What a drop turns into.
 *
 * These are the sums the board cannot show you it got wrong: a slot in a
 * column has to become a single index in one flat `entries` array, and an
 * off-by-one there moves the card somewhere plausible rather than somewhere
 * visibly broken. Each case here applies the patches to a model array and
 * checks the order that comes out, rather than asserting the indices directly
 * — the indices are the implementation, the order is the promise.
 */
import { describe, expect, it } from "vitest";
import { entryDropPatches, groupDropPatch, moveTarget, UNGROUPED } from "./reorder";
import type { Patch } from "../doc/patches";

type Entry = { id: string; group?: string | null };

const GROUPS = new Set(["play", "learn"]);

const ENTRIES: Entry[] = [
  { id: "a", group: "play" },
  { id: "b", group: "learn" },
  { id: "c", group: "play" },
  { id: "d" },
  { id: "e", group: "play" },
];

/** Apply patches the way `ConfigDoc` would, and report the resulting order. */
function applied(entries: Entry[], patches: Patch[]): Entry[] {
  let out = entries.map((e) => ({ ...e }));
  for (const patch of patches) {
    if (patch.op === "move") {
      const [moved] = out.splice(patch.from, 1);
      out.splice(patch.to, 0, moved);
    } else if (patch.op === "set") {
      const id = /entries\[id=(.+)\]\.group/.exec(patch.path)?.[1];
      out = out.map((e) => (e.id === id ? { ...e, group: patch.value as string } : e));
    } else if (patch.op === "unset") {
      const id = /entries\[id=(.+)\]\.group/.exec(patch.path)?.[1];
      out = out.map((e) => (e.id === id ? { id: e.id } : e));
    }
  }
  return out;
}

const ids = (entries: Entry[]) => entries.map((e) => e.id).join("");
const column = (entries: Entry[], group: string) =>
  entries
    .filter((e) => (e.group && GROUPS.has(e.group) ? e.group : UNGROUPED) === group)
    .map((e) => e.id)
    .join("");

/** Drop `id` into `col` at `slot` and report the whole resulting order. */
const drop = (id: string, col: string, slot: number, entries = ENTRIES) =>
  applied(entries, entryDropPatches(entries, GROUPS, id, col, slot));

describe("dropping an activity inside its own column", () => {
  it("moves it to the top", () => {
    expect(column(drop("e", "play", 0), "play")).toBe("eac");
  });

  it("moves it into the middle", () => {
    expect(column(drop("e", "play", 1), "play")).toBe("aec");
  });

  it("moves it to the bottom", () => {
    expect(column(drop("a", "play", 3), "play")).toBe("cea");
  });

  it("leaves the other columns alone", () => {
    const after = drop("a", "play", 3);
    expect(column(after, "learn")).toBe("b");
    expect(column(after, UNGROUPED)).toBe("d");
  });

  it("writes nothing for a drop either side of where it already is", () => {
    expect(entryDropPatches(ENTRIES, GROUPS, "c", "play", 1)).toEqual([]);
    expect(entryDropPatches(ENTRIES, GROUPS, "c", "play", 2)).toEqual([]);
  });
});

describe("dropping an activity into another column", () => {
  it("sets the category and the position together", () => {
    const after = drop("d", "play", 1);
    expect(column(after, "play")).toBe("adce");
    expect(column(after, UNGROUPED)).toBe("");
  });

  it("can land at the top of the new column", () => {
    expect(column(drop("b", "play", 0), "play")).toBe("bace");
  });

  it("can land at the bottom of the new column", () => {
    expect(column(drop("b", "play", 3), "play")).toBe("aceb");
  });

  it("clears the category when dropped among the uncategorised", () => {
    const after = drop("a", UNGROUPED, 0);
    expect(after.find((e) => e.id === "a")?.group).toBeUndefined();
    expect(column(after, UNGROUPED)).toBe("ad");
  });

  it("only changes the category when the new column is empty", () => {
    const entries: Entry[] = [{ id: "a", group: "play" }, { id: "b", group: "play" }];
    const patches = entryDropPatches(entries, GROUPS, "a", "learn", 0);
    expect(patches.map((p) => p.op)).toEqual(["set"]);
    expect(ids(applied(entries, patches))).toBe("ab");
  });
});

describe("edge cases", () => {
  it("ignores a drop of an activity that is not there", () => {
    expect(entryDropPatches(ENTRIES, GROUPS, "ghost", "play", 0)).toEqual([]);
  });

  it("treats a category that is not declared as no category", () => {
    const entries: Entry[] = [{ id: "a", group: "gone" }, { id: "b" }];
    // Both are drawn in the uncategorised column, so this is a reorder there.
    const after = applied(entries, entryDropPatches(entries, GROUPS, "b", UNGROUPED, 0));
    expect(ids(after)).toBe("ba");
  });
});

describe("moveTarget", () => {
  it("accounts for the element being lifted out first", () => {
    // Slot 4 of a 5-element array means "last", which is index 4 after the
    // element at 1 has been taken out — not 5.
    expect(moveTarget(1, 5)).toBe(4);
    expect(moveTarget(4, 0)).toBe(0);
  });

  it("reports the two slots that change nothing", () => {
    expect(moveTarget(2, 2)).toBeNull();
    expect(moveTarget(2, 3)).toBeNull();
  });
});

describe("reordering categories", () => {
  const groups = [{ id: "books" }, { id: "play" }, { id: "watch" }];
  const order = (patch: Patch | null) => {
    const out = groups.map((g) => g.id);
    if (patch?.op === "move") out.splice(patch.to, 0, ...out.splice(patch.from, 1));
    return out.join(" ");
  };

  it("moves one to the front", () => {
    expect(order(groupDropPatch(groups, "watch", 0))).toBe("watch books play");
  });

  it("moves one to the back", () => {
    expect(order(groupDropPatch(groups, "books", 3))).toBe("play watch books");
  });

  it("writes nothing for a drop either side of where it already is", () => {
    expect(groupDropPatch(groups, "play", 1)).toBeNull();
    expect(groupDropPatch(groups, "play", 2)).toBeNull();
  });

  it("ignores a category that is not there", () => {
    expect(groupDropPatch(groups, "ghost", 0)).toBeNull();
  });
});
