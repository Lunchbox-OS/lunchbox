/**
 * The tree itself (issue #195).
 *
 * A `treegrid`, which is the ARIA pattern for exactly this — a table whose
 * rows nest — and the reason each row carries `aria-level` and
 * `aria-expanded`. The caret is managed with `aria-activedescendant` rather
 * than by moving DOM focus row to row: one focusable container, no focus to
 * juggle when a folder's children arrive underneath it.
 */
import { useCallback, useRef, useState } from "react";
import {
  DndContext,
  DragOverlay,
  PointerSensor,
  TouchSensor,
  pointerWithin,
  useSensor,
  useSensors,
  type DragEndEvent,
  type DragOverEvent,
  type DragStartEvent,
} from "@dnd-kit/core";
import Table from "@mui/material/Table";
import TableBody from "@mui/material/TableBody";
import TableContainer from "@mui/material/TableContainer";
import Paper from "@mui/material/Paper";
import { FileRow } from "./FileRow";
import { FileTableHead } from "./FileTableHead";
import Paper2 from "@mui/material/Paper";
import Typography from "@mui/material/Typography";
import type { NodeKey, Row, SortSpec } from "./tree";
import { isSelectable } from "./tree";
import { isFileDrag, moveRefusal } from "./dnd";

export interface FileTreeTableProps {
  rows: Row[];
  compact: boolean;
  selected: NodeKey | null;
  /** The row whose name is being edited in place. */
  renaming: NodeKey | null;
  /** Folders with an upload arriving, for a spinner on the row. */
  busy: ReadonlySet<NodeKey>;
  sort: SortSpec;
  onSort: (column: SortSpec["column"]) => void;
  onSelect: (key: NodeKey | null) => void;
  onToggle: (key: NodeKey) => void;
  onActivate: (row: Row) => void;
  onMenu: (row: Row, anchor: HTMLElement) => void;
  onRetry: (node: NodeKey) => void;
  onShowMore: (node: NodeKey) => void;
  onRenameCommit: (row: Row, name: string) => void;
  onRenameCancel: () => void;
  onDropFiles: (row: Row, transfer: DataTransfer) => void;
  /** A row was dragged onto a folder. */
  onMove: (source: Row, target: Row) => void;
  /** A drag ended somewhere it could not go, with the reason. */
  onMoveRefused: (reason: string) => void;
  /** Open a folder a drag has hovered — the spring-loaded folder. */
  onExpand: (key: NodeKey) => void;
}

/** Row ids have to be stable and DOM-safe; the key is neither. */
function rowDomId(index: number): string {
  return `file-row-${index}`;
}

export function FileTreeTable({
  rows,
  compact,
  selected,
  renaming,
  busy,
  sort,
  onSort,
  onSelect,
  onToggle,
  onActivate,
  onMenu,
  onRetry,
  onShowMore,
  onRenameCommit,
  onRenameCancel,
  onDropFiles,
  onMove,
  onMoveRefused,
  onExpand,
}: FileTreeTableProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  // Which row a file drag is over, and whether one is happening at all. The
  // second is what draws the hint: a drag that lands between folders would
  // otherwise look like it should have worked.
  const [dropTarget, setDropTarget] = useState<NodeKey | null>(null);
  const [fileDrag, setFileDrag] = useState(false);
  // The row being dragged, for the overlay and for asking each folder whether
  // it would take it.
  const [dragging, setDragging] = useState<Row | null>(null);
  const springTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const sensors = useSensors(
    // A few pixels of movement before a drag starts, so a click on a row is
    // still a click and the ⋮ button is still reachable.
    useSensor(PointerSensor, { activationConstraint: { distance: 8 } }),
    // A long press on a phone, rather than stealing the scroll.
    useSensor(TouchSensor, { activationConstraint: { delay: 250, tolerance: 8 } }),
  );

  const cancelSpring = () => {
    if (springTimer.current) clearTimeout(springTimer.current);
    springTimer.current = null;
  };

  const onDragStart = (event: DragStartEvent) => {
    const row = event.active.data.current?.row as Row | undefined;
    setDragging(row ?? null);
  };

  /**
   * Spring-loaded folders: hovering a shut folder for a moment opens it, so a
   * drag can navigate downwards without being dropped and picked up again.
   */
  const onDragOver = (event: DragOverEvent) => {
    cancelSpring();
    const over = event.over?.data.current?.row as Row | undefined;
    if (!over || !isSelectable(over) || over.expanded) return;
    const key = over.key;
    springTimer.current = setTimeout(() => onExpand(key), 800);
  };

  const onDragEnd = (event: DragEndEvent) => {
    cancelSpring();
    const source = event.active.data.current?.row as Row | undefined;
    const target = event.over?.data.current?.row as Row | undefined;
    setDragging(null);
    if (!source || !target) return;
    const refusal = moveRefusal(source, target);
    // Checked again at the drop, not only at the highlight: the tree can have
    // moved under the drag, and "that is where it already is" deserves a
    // sentence rather than a silent no-op.
    if (refusal) onMoveRefused(refusal);
    else onMove(source, target);
  };
  const selectableRows = rows.filter(isSelectable);
  const selectedIndex = rows.findIndex((row) => row.key === selected);

  const move = useCallback(
    (delta: number) => {
      if (selectableRows.length === 0) return;
      const current = selectableRows.findIndex((row) => row.key === selected);
      const next = current < 0 ? 0 : current + delta;
      const clamped = Math.max(0, Math.min(selectableRows.length - 1, next));
      onSelect(selectableRows[clamped].key);
    },
    [selectableRows, selected, onSelect],
  );

  const onKeyDown = useCallback(
    (event: React.KeyboardEvent) => {
      // The inline rename field is inside a row; the tree must not eat its
      // arrows, its Enter or its Escape.
      if (renaming !== null) return;
      const row = rows.find((r) => r.key === selected);
      switch (event.key) {
        case "ArrowDown":
          event.preventDefault();
          move(1);
          break;
        case "ArrowUp":
          event.preventDefault();
          move(-1);
          break;
        case "ArrowRight":
          if (!row || !isSelectable(row)) return;
          event.preventDefault();
          // Closed folder opens; open folder steps into it, which is what the
          // next row already is.
          if (!row.expanded) onToggle(row.key);
          else move(1);
          break;
        case "ArrowLeft":
          if (!row || !isSelectable(row)) return;
          event.preventDefault();
          if (row.expanded) onToggle(row.key);
          else move(-1);
          break;
        case "Enter":
          if (!row) return;
          event.preventDefault();
          onActivate(row);
          break;
        default:
          break;
      }
    },
    [rows, selected, renaming, move, onToggle, onActivate],
  );

  return (
    <DndContext
      sensors={sensors}
      // Pointer-within rather than rectangle intersection: rows are wide and
      // short, and what a person means is the row under the cursor.
      collisionDetection={pointerWithin}
      onDragStart={onDragStart}
      onDragOver={onDragOver}
      onDragEnd={onDragEnd}
      onDragCancel={() => {
        cancelSpring();
        setDragging(null);
      }}
    >
    <TableContainer
      component={Paper}
      variant="outlined"
      ref={containerRef}
      tabIndex={0}
      role="treegrid"
      aria-label="Files on this device"
      aria-activedescendant={
        selectedIndex >= 0 ? rowDomId(selectedIndex) : undefined
      }
      onKeyDown={onKeyDown}
      onDragEnter={(e) => {
        if (isFileDrag(e.dataTransfer)) setFileDrag(true);
      }}
      onDragOver={(e) => {
        // Swallowed at the container as well as the row, so a drop between
        // rows is a no-op rather than the browser navigating to the file.
        if (isFileDrag(e.dataTransfer)) {
          e.preventDefault();
          e.dataTransfer.dropEffect = "none";
        }
      }}
      onDragLeave={(e) => {
        if (e.currentTarget === e.target) {
          setFileDrag(false);
          setDropTarget(null);
        }
      }}
      onDrop={(e) => {
        if (isFileDrag(e.dataTransfer)) e.preventDefault();
        setFileDrag(false);
        setDropTarget(null);
      }}
      sx={{ outline: "none", "&:focus-visible": { boxShadow: 2 } }}
    >
      {fileDrag && dropTarget === null ? (
        <Typography
          variant="body2"
          color="primary"
          sx={{ p: 1, textAlign: "center", bgcolor: "action.hover" }}
        >
          Drop onto a folder to upload into it
        </Typography>
      ) : null}
      <Table size="small" stickyHeader>
        {compact ? null : <FileTableHead sort={sort} onSort={onSort} />}
        <TableBody>
          {rows.map((row, index) => (
            <FileRow
              key={row.key}
              rowId={rowDomId(index)}
              row={row}
              compact={compact}
              selected={row.key === selected}
              renaming={row.key === renaming}
              dropTarget={row.key === dropTarget}
              busy={busy.has(row.key)}
              dragging={dragging}
              setSize={rows.length}
              positionInSet={index + 1}
              onSelect={(r) => onSelect(r.key)}
              onToggle={(r) => onToggle(r.key)}
              onActivate={onActivate}
              onMenu={onMenu}
              onRetry={onRetry}
              onShowMore={onShowMore}
              onRenameCommit={onRenameCommit}
              onRenameCancel={onRenameCancel}
              onDropFiles={(r, transfer) => {
                setFileDrag(false);
                onDropFiles(r, transfer);
              }}
              onDragOverRow={(r) => setDropTarget(r ? r.key : null)}
            />
          ))}
        </TableBody>
      </Table>
    </TableContainer>
    <DragOverlay dropAnimation={null}>
      {dragging ? (
        <Paper2
          elevation={4}
          sx={{ px: 1.5, py: 0.5, display: "inline-block", pointerEvents: "none" }}
        >
          <Typography variant="body2">{labelOf(dragging)}</Typography>
        </Paper2>
      ) : null}
    </DragOverlay>
    </DndContext>
  );
}

function labelOf(row: Row): string {
  if (row.kind === "root") return row.root.label;
  return row.kind === "entry" ? row.entry.name : "";
}
