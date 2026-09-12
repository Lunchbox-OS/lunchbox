/**
 * The ⋮ menu (issue #195).
 *
 * Not a convenience: it is the real interface, and the gestures are the
 * accelerator. A phone has no drag and a screen reader has no drop, so
 * everything the tree can do has to be reachable from here — which for now is
 * downloading a file and re-reading a folder, and later is rename, move and
 * delete.
 */
import Menu from "@mui/material/Menu";
import MenuItem from "@mui/material/MenuItem";
import ListItemIcon from "@mui/material/ListItemIcon";
import ListItemText from "@mui/material/ListItemText";
import DownloadIcon from "@mui/icons-material/Download";
import RefreshIcon from "@mui/icons-material/Refresh";
import type { Row } from "./tree";

export interface RowActionsMenuProps {
  row: Row | null;
  anchor: HTMLElement | null;
  onClose: () => void;
  onDownload: (row: Row) => void;
  onRefresh: (row: Row) => void;
}

export function RowActionsMenu({
  row,
  anchor,
  onClose,
  onDownload,
  onRefresh,
}: RowActionsMenuProps) {
  const entry = row?.kind === "entry" ? row.entry : null;
  const isFolder = entry?.kind === "dir";
  // An escaping symlink or a name that is not addressable: listed so it can be
  // seen, and nothing here will open it.
  const usable = entry !== null && entry.unusable === undefined;

  return (
    <Menu anchorEl={anchor} open={Boolean(anchor && row)} onClose={onClose}>
      {!isFolder && (
        <MenuItem
          disabled={!usable}
          onClick={() => {
            if (row) onDownload(row);
            onClose();
          }}
        >
          <ListItemIcon>
            <DownloadIcon fontSize="small" />
          </ListItemIcon>
          <ListItemText>Download</ListItemText>
        </MenuItem>
      )}
      {isFolder && (
        <MenuItem
          disabled={!usable}
          onClick={() => {
            if (row) onRefresh(row);
            onClose();
          }}
        >
          <ListItemIcon>
            <RefreshIcon fontSize="small" />
          </ListItemIcon>
          <ListItemText>Re-read this folder</ListItemText>
        </MenuItem>
      )}
    </Menu>
  );
}
