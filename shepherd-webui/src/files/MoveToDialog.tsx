/**
 * "Move to…" (issue #195).
 *
 * The reason the drag is an accelerator rather than the interface: a phone has
 * no drag worth the name and a screen reader has no drop. Everything the
 * gesture does is reachable here, from the ⋮ menu.
 *
 * It is the same tree, reduced to what can be a destination — its own
 * expansion state, the same per-directory queries (so a folder already open
 * behind the dialog costs nothing to show), and folders only, because a file
 * cannot be one.
 */
import { useMemo } from "react";
import Alert from "@mui/material/Alert";
import Box from "@mui/material/Box";
import Button from "@mui/material/Button";
import CircularProgress from "@mui/material/CircularProgress";
import Dialog from "@mui/material/Dialog";
import DialogActions from "@mui/material/DialogActions";
import DialogContent from "@mui/material/DialogContent";
import DialogTitle from "@mui/material/DialogTitle";
import IconButton from "@mui/material/IconButton";
import Typography from "@mui/material/Typography";
import ChevronRightIcon from "@mui/icons-material/ChevronRight";
import ExpandMoreIcon from "@mui/icons-material/ExpandMore";
import FolderIcon from "@mui/icons-material/Folder";
import HomeIcon from "@mui/icons-material/Home";
import PlaceIcon from "@mui/icons-material/Place";
import UsbIcon from "@mui/icons-material/Usb";
import { moveRefusal } from "./dnd";
import { buildRows, reachableExpanded, type Row } from "./tree";
import { useDirectories } from "./useDirectories";
import { useFileTree } from "./useFileTree";
import type { RootInfo } from "./types";

export interface MoveToDialogProps {
  open: boolean;
  /** What is being moved. `null` closes the dialog. */
  source: Row | null;
  roots: RootInfo[];
  busy: boolean;
  error: string | null;
  onCancel: () => void;
  onMove: (target: Row) => void;
}

export function MoveToDialog({
  open,
  source,
  roots,
  busy,
  error,
  onCancel,
  onMove,
}: MoveToDialogProps) {
  // Its own expansion, so opening a folder to look for a destination does not
  // rearrange the tree behind the dialog.
  const tree = useFileTree();

  // One place only: a move cannot cross roots, so offering the others would be
  // offering a refusal.
  const sourceRoot = source?.kind === "entry" ? source.rootId : null;
  const candidates = useMemo(
    () => roots.filter((root) => root.id === sourceRoot),
    [roots, sourceRoot],
  );

  const openKeys = useMemo(
    () => reachableExpanded(tree.state.expanded, candidates),
    [tree.state.expanded, candidates],
  );
  const directories = useDirectories(openKeys, tree.state.pageLimits);

  const rows = useMemo(
    () =>
      buildRows({
        roots: candidates,
        expanded: tree.state.expanded,
        directories,
        sort: { column: "name", direction: "asc" },
        foldersFirst: true,
        showHidden: tree.state.showHidden,
        foldersOnly: true,
      }),
    [candidates, directories, tree.state],
  );

  const selected = rows.find((row) => row.key === tree.state.selected) ?? null;
  const refusal = source && selected ? moveRefusal(source, selected) : null;
  const name = source?.kind === "entry" ? source.entry.name : "";

  return (
    <Dialog open={open} onClose={onCancel} fullWidth maxWidth="sm">
      <DialogTitle>Move {name} to…</DialogTitle>
      <DialogContent dividers sx={{ minHeight: 280 }}>
        {error && (
          <Alert severity="error" sx={{ mb: 2 }}>
            {error}
          </Alert>
        )}
        <Box role="tree" aria-label="Choose a folder">
          {rows.map((row) => (
            <PickerRow
              key={row.key}
              row={row}
              selected={row.key === tree.state.selected}
              onSelect={() => tree.select(row.key)}
              onToggle={() => tree.toggle(row.key)}
            />
          ))}
        </Box>
      </DialogContent>
      <DialogActions sx={{ justifyContent: "space-between" }}>
        <Typography variant="caption" color="text.secondary" sx={{ pl: 2 }}>
          {refusal ?? (selected ? " " : "Choose a folder.")}
        </Typography>
        <Box>
          <Button onClick={onCancel}>Cancel</Button>
          <Button
            variant="contained"
            disabled={busy || !selected || refusal !== null}
            onClick={() => selected && onMove(selected)}
          >
            {busy ? "Moving…" : "Move"}
          </Button>
        </Box>
      </DialogActions>
    </Dialog>
  );
}

function PickerRow({
  row,
  selected,
  onSelect,
  onToggle,
}: {
  row: Row;
  selected: boolean;
  onSelect: () => void;
  onToggle: () => void;
}) {
  if (row.kind === "pending") {
    return (
      <Indented depth={row.depth}>
        <CircularProgress size={14} sx={{ mr: 1 }} />
        <Typography variant="body2" color="text.secondary">
          Reading…
        </Typography>
      </Indented>
    );
  }
  if (row.kind === "empty") {
    return (
      <Indented depth={row.depth}>
        <Typography variant="body2" color="text.secondary">
          No folders in here
        </Typography>
      </Indented>
    );
  }
  if (row.kind !== "root" && row.kind !== "entry") return null;

  const name = row.kind === "root" ? row.root.label : row.entry.name;
  const Icon =
    row.kind === "entry"
      ? FolderIcon
      : row.root.kind === "home"
        ? HomeIcon
        : row.root.kind === "external"
          ? UsbIcon
          : PlaceIcon;

  return (
    <Box
      role="treeitem"
      aria-level={row.depth + 1}
      aria-selected={selected}
      aria-expanded={row.expanded}
      onClick={onSelect}
      sx={{
        display: "flex",
        alignItems: "center",
        gap: 0.5,
        pl: `${row.depth * 18}px`,
        py: 0.25,
        borderRadius: 1,
        cursor: "pointer",
        bgcolor: selected ? "action.selected" : undefined,
        "&:hover": { bgcolor: selected ? "action.selected" : "action.hover" },
      }}
    >
      <IconButton
        size="small"
        aria-label={row.expanded ? `Collapse ${name}` : `Expand ${name}`}
        onClick={(e) => {
          e.stopPropagation();
          onToggle();
        }}
      >
        {row.expanded ? (
          <ExpandMoreIcon fontSize="small" />
        ) : (
          <ChevronRightIcon fontSize="small" />
        )}
      </IconButton>
      <Icon fontSize="small" sx={{ color: row.kind === "entry" ? "primary.light" : undefined }} />
      <Typography variant="body2" noWrap sx={{ fontWeight: row.kind === "root" ? 600 : 400 }}>
        {name}
      </Typography>
    </Box>
  );
}

function Indented({ depth, children }: { depth: number; children: React.ReactNode }) {
  return (
    <Box sx={{ display: "flex", alignItems: "center", pl: `${depth * 18 + 34}px`, py: 0.25 }}>
      {children}
    </Box>
  );
}
