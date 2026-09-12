/**
 * The tree itself (issue #195).
 *
 * A `treegrid`, which is the ARIA pattern for exactly this — a table whose
 * rows nest — and the reason each row carries `aria-level` and
 * `aria-expanded`. The caret is managed with `aria-activedescendant` rather
 * than by moving DOM focus row to row: one focusable container, no focus to
 * juggle when a folder's children arrive underneath it.
 */
import { useCallback, useRef } from "react";
import Table from "@mui/material/Table";
import TableBody from "@mui/material/TableBody";
import TableContainer from "@mui/material/TableContainer";
import Paper from "@mui/material/Paper";
import { FileRow } from "./FileRow";
import { FileTableHead } from "./FileTableHead";
import type { NodeKey, Row, SortSpec } from "./tree";
import { isSelectable } from "./tree";

export interface FileTreeTableProps {
  rows: Row[];
  compact: boolean;
  selected: NodeKey | null;
  sort: SortSpec;
  onSort: (column: SortSpec["column"]) => void;
  onSelect: (key: NodeKey | null) => void;
  onToggle: (key: NodeKey) => void;
  onActivate: (row: Row) => void;
  onMenu: (row: Row, anchor: HTMLElement) => void;
  onRetry: (node: NodeKey) => void;
  onShowMore: (node: NodeKey) => void;
}

/** Row ids have to be stable and DOM-safe; the key is neither. */
function rowDomId(index: number): string {
  return `file-row-${index}`;
}

export function FileTreeTable({
  rows,
  compact,
  selected,
  sort,
  onSort,
  onSelect,
  onToggle,
  onActivate,
  onMenu,
  onRetry,
  onShowMore,
}: FileTreeTableProps) {
  const containerRef = useRef<HTMLDivElement>(null);
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
    [rows, selected, move, onToggle, onActivate],
  );

  return (
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
      sx={{ outline: "none", "&:focus-visible": { boxShadow: 2 } }}
    >
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
              setSize={rows.length}
              positionInSet={index + 1}
              onSelect={(r) => onSelect(r.key)}
              onToggle={(r) => onToggle(r.key)}
              onActivate={onActivate}
              onMenu={onMenu}
              onRetry={onRetry}
              onShowMore={onShowMore}
            />
          ))}
        </TableBody>
      </Table>
    </TableContainer>
  );
}
