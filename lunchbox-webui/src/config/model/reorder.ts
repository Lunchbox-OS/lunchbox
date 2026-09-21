/**
 * Turning a drop into patches.
 *
 * Order in the config is order on the home screen: the launcher draws one
 * compartment per group in the order `[[groups]]` are declared, each holding
 * its members in the order `[[entries]]` are declared (issue #210). Until now
 * the only way to change either was to edit the TOML by hand.
 *
 * The awkward part is that `entries` is one flat array while the board shows
 * it as a column per category. A drop names a *column* and a *slot inside that
 * column*, and this works out which single position in the flat array that
 * corresponds to — so the file keeps one entry per `[[entries]]` block in the
 * order they are drawn, rather than growing an explicit ordering field.
 *
 * Kept apart from the board itself because this is where reordering can
 * actually be wrong, and it is testable without a pointer.
 */
import { entryPath, move, set, unset, type Patch } from "../doc/patches";

/** The column for activities with no category, or one that no longer exists. */
export const UNGROUPED = "__ungrouped__";

/**
 * Which column an entry is drawn in. An entry naming a category that is not
 * declared falls in with the uncategorised, which is what the launcher does
 * with it too.
 */
export function columnOf(
  entry: { group?: string | null },
  knownGroupIds: ReadonlySet<string>,
): string {
  return entry.group && knownGroupIds.has(entry.group) ? entry.group : UNGROUPED;
}

/**
 * Where an element lands in `move`, which lifts it out before putting it back.
 *
 * `before` is an index into the array as it stands — "insert ahead of whatever
 * is here now", with `length` meaning the end. Returns null when that is where
 * the element already is, so a drop either side of a card is a no-op rather
 * than an edit that writes the file and costs an undo step.
 */
export function moveTarget(from: number, before: number): number | null {
  if (before === from || before === from + 1) return null;
  return before > from ? before - 1 : before;
}

/**
 * The patches for dropping `entryId` into `column` at `slot`.
 *
 * `slot` counts the gaps in the column as drawn, so it runs from 0 (above the
 * first card) to the number of cards (below the last) — and the column is
 * drawn with the dragged card still in it, since nothing has changed yet.
 *
 * Both halves of a move across columns — the new `group`, the new position —
 * come back together so the caller can apply them under one coalesce key. A
 * drop that changes neither returns nothing.
 */
export function entryDropPatches(
  entries: readonly { id: string; group?: string | null }[],
  known: ReadonlySet<string>,
  entryId: string,
  column: string,
  slot: number,
): Patch[] {
  const from = entries.findIndex((e) => e.id === entryId);
  if (from < 0) return [];
  const dragged = entries[from];

  const patches: Patch[] = [];
  if (columnOf(dragged, known) !== column) {
    const path = entryPath(entryId, "group");
    patches.push(column === UNGROUPED ? unset(path) : set(path, column));
  }

  // The column as it will be once the card is out of it. Taking the card out
  // first is what lets one slot number mean the same thing whichever column
  // the drag started in: `landing` is always an insertion point among the
  // cards that are staying put.
  const drawn = entries.filter((e) => columnOf(e, known) === column);
  const here = drawn.findIndex((e) => e.id === entryId);
  const rest = here < 0 ? drawn : [...drawn.slice(0, here), ...drawn.slice(here + 1)];
  const landing = here >= 0 && slot > here ? slot - 1 : slot;

  // Both gaps touching a card put it back exactly where it was. Worth catching
  // here rather than leaving to `moveTarget`, because the two entries either
  // side of a card in its column need not be either side of it in the file —
  // other columns' entries sit in between — so the flat move would be real
  // even though the order on screen would not change at all.
  if (here >= 0 && landing === here) return patches;

  const before = flatIndexFor(entries, rest, landing);
  // An empty column has nothing to sit between, so the entry stays where it is
  // in the file and only its `group` changes. Moving it to the end would be a
  // large diff for an order nobody can see.
  if (before !== null) {
    const to = moveTarget(from, before);
    if (to !== null) patches.push(move("entries", from, to));
  }
  return patches;
}

/**
 * The index in the flat array that `slot` in a column points at, or null when
 * the column has no other members to place this one against.
 */
function flatIndexFor(
  entries: readonly { id: string }[],
  rest: readonly { id: string }[],
  slot: number,
): number | null {
  if (rest.length === 0) return null;
  const indexOf = (id: string) => entries.findIndex((e) => e.id === id);
  if (slot < rest.length) return indexOf(rest[slot].id);
  return indexOf(rest[rest.length - 1].id) + 1;
}

/**
 * The patch for dropping `groupId` at `slot` in the category list, or null if
 * that leaves the order alone.
 */
export function groupDropPatch(
  groups: readonly { id: string }[],
  groupId: string,
  slot: number,
): Patch | null {
  const from = groups.findIndex((g) => g.id === groupId);
  if (from < 0) return null;
  const to = moveTarget(from, slot);
  return to === null ? null : move("groups", from, to);
}
