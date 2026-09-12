/**
 * One row of the tree (issue #195).
 *
 * Memoised and dumb: it is handed a row and some callbacks and reaches into
 * nothing. That is what keeps a windowing wrapper a later change rather than a
 * rewrite, and it is why the whole tree can be a flat array.
 */
import { memo } from "react";
import Box from "@mui/material/Box";
import CircularProgress from "@mui/material/CircularProgress";
import IconButton from "@mui/material/IconButton";
import Link from "@mui/material/Link";
import TableCell from "@mui/material/TableCell";
import TableRow from "@mui/material/TableRow";
import Tooltip from "@mui/material/Tooltip";
import Typography from "@mui/material/Typography";
import ChevronRightIcon from "@mui/icons-material/ChevronRight";
import ExpandMoreIcon from "@mui/icons-material/ExpandMore";
import FolderIcon from "@mui/icons-material/Folder";
import HomeIcon from "@mui/icons-material/Home";
import InsertDriveFileOutlinedIcon from "@mui/icons-material/InsertDriveFileOutlined";
import LinkOffIcon from "@mui/icons-material/LinkOff";
import LockIcon from "@mui/icons-material/Lock";
import MoreVertIcon from "@mui/icons-material/MoreVert";
import PlaceIcon from "@mui/icons-material/Place";
import UsbIcon from "@mui/icons-material/Usb";
import type { Row } from "./tree";
import { isExpandable } from "./tree";
import {
  formatBytes,
  formatModified,
  formatModifiedFull,
  formatRootSpace,
  kindLabel,
  rootKindLabel,
  unusableLabel,
} from "./format";

/** One indent step, in pixels. Deep trees stay readable; shallow ones scan. */
const INDENT = 18;

export interface FileRowProps {
  row: Row;
  compact: boolean;
  selected: boolean;
  rowId: string;
  /** Total rows, for `aria-setsize` — a treegrid row has to say where it sits. */
  setSize: number;
  positionInSet: number;
  onSelect: (row: Row) => void;
  onToggle: (row: Row) => void;
  onActivate: (row: Row) => void;
  onMenu: (row: Row, anchor: HTMLElement) => void;
  onRetry: (node: string) => void;
  onShowMore: (node: string) => void;
}

function FileRowImpl({
  row,
  compact,
  selected,
  rowId,
  setSize,
  positionInSet,
  onSelect,
  onToggle,
  onActivate,
  onMenu,
  onRetry,
  onShowMore,
}: FileRowProps) {
  // The rows that are not nodes: a directory's state, rendered at the depth
  // its children would have had so the tree does not jump when they arrive.
  if (row.kind === "pending") {
    return (
      <FillerRow depth={row.depth} compact={compact}>
        <CircularProgress size={14} sx={{ mr: 1 }} />
        Reading…
      </FillerRow>
    );
  }
  if (row.kind === "empty") {
    return (
      <FillerRow depth={row.depth} compact={compact}>
        Nothing in here
      </FillerRow>
    );
  }
  if (row.kind === "error") {
    return (
      <FillerRow depth={row.depth} compact={compact}>
        {row.message}{" "}
        <Link component="button" variant="body2" onClick={() => onRetry(row.node)}>
          Try again
        </Link>
      </FillerRow>
    );
  }
  if (row.kind === "more") {
    return (
      <FillerRow depth={row.depth} compact={compact}>
        Showing the first {row.shown}. Sorting is by name in this folder.{" "}
        <Link
          component="button"
          variant="body2"
          onClick={() => onShowMore(row.node)}
        >
          Show more
        </Link>
      </FillerRow>
    );
  }

  const isRoot = row.kind === "root";
  const entry = isRoot ? null : row.entry;
  const unusable = entry?.unusable;
  const expandable = isRoot || (entry !== null && isExpandable(entry));
  const name = isRoot ? row.root.label : row.entry.name;
  const hidden = !isRoot && row.entry.hidden;

  return (
    <TableRow
      id={rowId}
      hover
      selected={selected}
      aria-level={row.depth + 1}
      aria-posinset={positionInSet}
      aria-setsize={setSize}
      aria-expanded={expandable ? row.expanded : undefined}
      onClick={() => onSelect(row)}
      onDoubleClick={() => onActivate(row)}
      sx={{
        cursor: "default",
        // A row that cannot be acted on should look it, without disappearing:
        // the escaping symlink is listed precisely so somebody can see it.
        opacity: unusable ? 0.55 : hidden ? 0.7 : 1,
        "& td": { py: compact ? 1 : 0.25 },
      }}
    >
      <TableCell sx={{ pl: `${8 + row.depth * INDENT}px` }}>
        <Box sx={{ display: "flex", alignItems: "center", gap: 0.5, minWidth: 0 }}>
          <Box sx={{ width: 24, display: "flex", justifyContent: "center" }}>
            {expandable ? (
              <IconButton
                size="small"
                aria-label={row.expanded ? `Collapse ${name}` : `Expand ${name}`}
                onClick={(e) => {
                  e.stopPropagation();
                  onToggle(row);
                }}
              >
                {row.expanded ? (
                  <ExpandMoreIcon fontSize="small" />
                ) : (
                  <ChevronRightIcon fontSize="small" />
                )}
              </IconButton>
            ) : null}
          </Box>
          <RowIcon row={row} />
          <Box sx={{ minWidth: 0 }}>
            <Typography
              variant="body2"
              noWrap
              sx={{ fontWeight: isRoot ? 600 : 400 }}
              title={isRoot ? row.root.path : name}
            >
              {name}
            </Typography>
            {compact ? (
              <Typography variant="caption" color="text.secondary" noWrap>
                {isRoot
                  ? formatRootSpace(row.root)
                  : [
                      formatBytes(row.entry.size),
                      formatModified(row.entry.modified),
                    ]
                      .filter(Boolean)
                      .join(" · ")}
              </Typography>
            ) : null}
          </Box>
          {unusable ? (
            <Tooltip title={unusableLabel(unusable)}>
              <LinkOffIcon fontSize="small" color="disabled" />
            </Tooltip>
          ) : null}
          {isRoot && !row.root.writable ? (
            <Tooltip title="This place is read-only on this device.">
              <LockIcon fontSize="small" color="disabled" />
            </Tooltip>
          ) : null}
        </Box>
      </TableCell>

      {compact ? null : (
        <>
          <TableCell align="right">
            <Typography variant="body2" color="text.secondary" noWrap>
              {isRoot ? formatRootSpace(row.root) : formatBytes(row.entry.size)}
            </Typography>
          </TableCell>
          <TableCell>
            <Typography variant="body2" color="text.secondary" noWrap>
              {isRoot ? rootKindLabel(row.root) : kindLabel(row.entry)}
            </Typography>
          </TableCell>
          <TableCell>
            <Tooltip
              title={isRoot ? "" : formatModifiedFull(row.entry.modified)}
            >
              <Typography variant="body2" color="text.secondary" noWrap>
                {isRoot ? "" : formatModified(row.entry.modified)}
              </Typography>
            </Tooltip>
          </TableCell>
        </>
      )}

      <TableCell align="right" padding="none" sx={{ pr: 1 }}>
        {isRoot ? null : (
          <IconButton
            size="small"
            aria-label={`Actions for ${name}`}
            onClick={(e) => {
              e.stopPropagation();
              onMenu(row, e.currentTarget);
            }}
          >
            <MoreVertIcon fontSize="small" />
          </IconButton>
        )}
      </TableCell>
    </TableRow>
  );
}

function RowIcon({ row }: { row: Row }) {
  if (row.kind === "root") {
    const Icon =
      row.root.kind === "home"
        ? HomeIcon
        : row.root.kind === "external"
          ? UsbIcon
          : PlaceIcon;
    return <Icon fontSize="small" color="action" />;
  }
  if (row.kind !== "entry") return null;
  return row.entry.kind === "dir" ? (
    <FolderIcon fontSize="small" sx={{ color: "primary.light" }} />
  ) : (
    <InsertDriveFileOutlinedIcon fontSize="small" color="action" />
  );
}

/** A row that says what a directory is doing rather than what is in it. */
function FillerRow({
  depth,
  compact,
  children,
}: {
  depth: number;
  compact: boolean;
  children: React.ReactNode;
}) {
  return (
    <TableRow>
      <TableCell
        colSpan={compact ? 2 : 5}
        sx={{ pl: `${8 + depth * INDENT + 24}px`, py: 0.5, border: 0 }}
      >
        <Typography
          variant="body2"
          color="text.secondary"
          sx={{ display: "flex", alignItems: "center" }}
        >
          {children}
        </Typography>
      </TableCell>
    </TableRow>
  );
}

export const FileRow = memo(FileRowImpl);
