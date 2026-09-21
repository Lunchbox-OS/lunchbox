/**
 * The space between two cards, as a thing you can drop onto.
 *
 * Order in the config is order on the home screen (issue #210), and the way to
 * say where something goes is to put it *between* two others — so the gap is
 * the target, not the card. `@dnd-kit` has no sortable list here (only its core
 * is a dependency, and the files browser already builds on that), so the gaps
 * are ordinary droppables laid out in the flow.
 *
 * They are always present, at the width of the spacing they replace, so the
 * board does not reflow when a drag starts; the bar inside one only takes
 * colour when it is the one that would receive the drop.
 */
import Box from "@mui/material/Box";
import {
  closestCenter,
  useDroppable,
  type CollisionDetection,
  type DroppableContainer,
} from "@dnd-kit/core";

/** What a gap or a column hands back through `over.data.current`. */
export type DropData = { kind: "gap"; column: string; slot: number } | { kind: "column"; column: string };

/** The gap above card `slot` in `column`; `slot === length` is below the last. */
export function DropGap({
  column,
  slot,
  /** Fill the rest of the column, so a drop in its empty space lands here. */
  grow = false,
}: {
  column: string;
  slot: number;
  grow?: boolean;
}) {
  const { setNodeRef, isOver } = useDroppable({
    id: `gap|${column}|${slot}`,
    data: { kind: "gap", column, slot } satisfies DropData,
  });
  return (
    <Box
      ref={setNodeRef}
      sx={{
        display: "flex",
        alignItems: "center",
        minHeight: grow ? 20 : 8,
        flexGrow: grow ? 1 : 0,
        flexShrink: 0,
      }}
    >
      {/* Wider than the cards on purpose: the card being dragged is the same
          width and sits under the pointer, so a bar that stopped at the card
          edges would be hidden by it exactly when it is being aimed. */}
      <Box
        sx={{
          height: 4,
          width: "calc(100% + 20px)",
          mx: "-10px",
          borderRadius: 2,
          backgroundColor: isOver ? "primary.main" : "transparent",
        }}
      />
    </Box>
  );
}

/**
 * Register a column so a drag can be scoped to it. Nothing is dropped *on* a
 * column — its rect is only there to decide which set of gaps to choose from.
 */
export function useDropColumn(column: string) {
  return useDroppable({ id: `column|${column}`, data: { kind: "column", column } satisfies DropData });
}

const dataOf = (container: DroppableContainer) => container.data.current as DropData | undefined;

/**
 * Pick the nearest gap *in the column being dragged over*.
 *
 * Plain `closestCenter` over every gap gets this wrong on a board whose
 * columns are different lengths: hold a card over the empty lower half of a
 * short column and the nearest gap by distance belongs to the tall column
 * beside it, so the card silently changes category. Choosing the column first
 * and the gap second is how the eye reads it.
 *
 * Measured from the dragged card rather than from the pointer, so it works the
 * same for a keyboard drag, which has no pointer.
 */
export const gapCollision: CollisionDetection = (args) => {
  const gaps = args.droppableContainers.filter((c) => dataOf(c)?.kind === "gap");
  const midpoint = args.collisionRect.left + args.collisionRect.width / 2;
  const column = args.droppableContainers.find((c) => {
    if (dataOf(c)?.kind !== "column") return false;
    const rect = args.droppableRects.get(c.id);
    return !!rect && midpoint >= rect.left && midpoint <= rect.right;
  });
  const scoped = column
    ? gaps.filter((c) => dataOf(c)?.column === dataOf(column)?.column)
    : [];
  return closestCenter({
    ...args,
    droppableContainers: scoped.length > 0 ? scoped : gaps,
  });
};

/** The gap a drag ended on, if it ended on one. */
export function droppedGap(over: { data: { current?: unknown } } | null | undefined) {
  const data = over?.data.current as DropData | undefined;
  return data?.kind === "gap" ? data : null;
}
