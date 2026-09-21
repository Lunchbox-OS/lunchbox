/**
 * Which gap a drag is aiming at.
 *
 * The case this exists for is a board whose columns are different lengths.
 * Plain `closestCenter` over every gap on the board answers "the nearest one
 * anywhere", and over the empty lower half of a short column the nearest one
 * anywhere belongs to the tall column beside it — so a card dropped where the
 * eye says "the end of this column" silently changes category instead.
 *
 * A layout is spelled out here in rectangles rather than rendered, because
 * none of this is about the DOM: it is arithmetic over rects that jsdom would
 * only report as zero anyway.
 */
import { describe, expect, it } from "vitest";
import { closestCenter, type CollisionDetection, type ClientRect } from "@dnd-kit/core";
import { gapCollision, type DropData } from "./DropGap";

const rect = (left: number, top: number, width: number, height: number): ClientRect => ({
  left,
  top,
  width,
  height,
  right: left + width,
  bottom: top + height,
});

/**
 * Two columns side by side. "short" holds one card, "tall" holds five, so
 * their trailing gaps are at very different heights.
 */
const LAYOUT: { id: string; data: DropData; rect: ClientRect }[] = [
  { id: "column|short", data: { kind: "column", column: "short" }, rect: rect(0, 0, 280, 600) },
  { id: "column|tall", data: { kind: "column", column: "tall" }, rect: rect(300, 0, 280, 600) },
  { id: "gap|short|0", data: { kind: "gap", column: "short", slot: 0 }, rect: rect(0, 40, 280, 8) },
  // The trailing gap fills the rest of its column.
  { id: "gap|short|1", data: { kind: "gap", column: "short", slot: 1 }, rect: rect(0, 118, 280, 480) },
  ...[0, 1, 2, 3, 4].map((slot) => ({
    id: `gap|tall|${slot}`,
    data: { kind: "gap" as const, column: "tall", slot },
    rect: rect(300, 40 + slot * 78, 280, 8),
  })),
  { id: "gap|tall|5", data: { kind: "gap", column: "tall", slot: 5 }, rect: rect(300, 430, 280, 168) },
];

/** Run a detection with the dragged card's rect at (left, top). */
function aim(
  detect: CollisionDetection,
  left: number,
  top: number,
  layout = LAYOUT,
): string {
  const args = {
    active: { id: "dragged", data: { current: undefined }, rect: { current: {} } },
    collisionRect: rect(left, top, 260, 60),
    droppableRects: new Map(layout.map((d) => [d.id, d.rect])),
    droppableContainers: layout.map((d) => ({
      id: d.id,
      data: { current: d.data },
      rect: { current: d.rect },
    })),
    pointerCoordinates: { x: left + 130, y: top + 30 },
  } as unknown as Parameters<CollisionDetection>[0];
  return String(detect(args)[0]?.id);
}

const aimAt = (left: number, top: number) => aim(gapCollision, left, top);
/** What plain distance over the gaps alone would say. */
const naivelyAt = (left: number, top: number) =>
  aim(closestCenter, left, top, LAYOUT.filter((d) => d.data.kind === "gap"));

describe("gapCollision", () => {
  it("picks the gap the card is nearest inside its own column", () => {
    expect(aimAt(310, 100)).toBe("gap|tall|1");
    expect(aimAt(310, 260)).toBe("gap|tall|3");
  });

  it("stays in the short column over its empty space", () => {
    // Below the short column's one card. Its own trailing gap is a tall one,
    // so its centre is a long way down; the tall column's gaps are small and
    // one of them has its centre right alongside. Distance alone therefore
    // sends the card across, and asserting that first is what keeps this case
    // honest: it is a test that fails without the column step.
    expect(naivelyAt(140, 170)).toMatch(/^gap\|tall\|/);
    expect(aimAt(140, 170)).toMatch(/^gap\|short\|/);
  });

  it("follows the card across to the other column", () => {
    expect(aimAt(310, 330)).toBe("gap|tall|4");
  });

  it("never offers a column itself as a target", () => {
    for (const top of [0, 100, 300, 590]) {
      for (const left of [10, 310]) {
        expect(aimAt(left, top)).toMatch(/^gap\|/);
      }
    }
  });

  it("falls back to the nearest gap anywhere when the card is off the board", () => {
    // Dragged out past the right-hand edge: no column contains its midpoint,
    // so rather than refusing the drop it offers the closest gap there is.
    expect(aimAt(900, 100)).toMatch(/^gap\|tall\|/);
  });
});
