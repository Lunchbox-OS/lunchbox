/** The sortable column headers (issue #195). Desktop only — see `FilesPage`. */
import TableCell from "@mui/material/TableCell";
import TableHead from "@mui/material/TableHead";
import TableRow from "@mui/material/TableRow";
import TableSortLabel from "@mui/material/TableSortLabel";
import type { SortColumn, SortSpec } from "./tree";

const COLUMNS: {
  id: SortColumn;
  label: string;
  align?: "right";
  width?: number;
}[] = [
  { id: "name", label: "Name" },
  { id: "size", label: "Size", align: "right", width: 140 },
  { id: "kind", label: "Kind", width: 140 },
  { id: "modified", label: "Modified", width: 160 },
];

export function FileTableHead({
  sort,
  onSort,
}: {
  sort: SortSpec;
  onSort: (column: SortColumn) => void;
}) {
  return (
    <TableHead>
      <TableRow>
        {COLUMNS.map((column) => (
          <TableCell
            key={column.id}
            align={column.align}
            sortDirection={sort.column === column.id ? sort.direction : false}
            sx={{ width: column.width }}
          >
            <TableSortLabel
              active={sort.column === column.id}
              direction={sort.column === column.id ? sort.direction : "asc"}
              onClick={() => onSort(column.id)}
            >
              {column.label}
            </TableSortLabel>
          </TableCell>
        ))}
        {/* The ⋮ column. Deliberately unlabelled, and narrow. */}
        <TableCell sx={{ width: 48 }} />
      </TableRow>
    </TableHead>
  );
}
