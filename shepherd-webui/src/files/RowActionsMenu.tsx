/**
 * The ⋮ menu (issue #195).
 *
 * Not a convenience: it is the real interface, and the gestures are the
 * accelerator. A phone has no drag and a screen reader has no drop, so
 * everything the tree can do is reachable from here — upload into a folder,
 * make one, rename, delete, download.
 *
 * Every item is enabled from [`permissions`], which reads what the device
 * already said about the row. A menu item that 403s is worse than one that is
 * not there.
 */
import Divider from "@mui/material/Divider";
import ListItemIcon from "@mui/material/ListItemIcon";
import ListItemText from "@mui/material/ListItemText";
import Menu from "@mui/material/Menu";
import MenuItem from "@mui/material/MenuItem";
import CreateNewFolderIcon from "@mui/icons-material/CreateNewFolder";
import DeleteOutlineIcon from "@mui/icons-material/DeleteOutlined";
import DownloadIcon from "@mui/icons-material/Download";
import DriveFileMoveIcon from "@mui/icons-material/DriveFileMove";
import DriveFileRenameOutlineIcon from "@mui/icons-material/DriveFileRenameOutline";
import RefreshIcon from "@mui/icons-material/Refresh";
import UploadFileIcon from "@mui/icons-material/UploadFile";
import type { Row } from "./tree";
import { canDelete, canDownload, canRename, canWriteInto } from "./permissions";

export interface RowActionsMenuProps {
  row: Row | null;
  anchor: HTMLElement | null;
  onClose: () => void;
  onDownload: (row: Row) => void;
  onRefresh: (row: Row) => void;
  onUploadInto: (row: Row) => void;
  onNewFolder: (row: Row) => void;
  onRename: (row: Row) => void;
  onMove: (row: Row) => void;
  onDelete: (row: Row) => void;
}

export function RowActionsMenu({
  row,
  anchor,
  onClose,
  onDownload,
  onRefresh,
  onUploadInto,
  onNewFolder,
  onRename,
  onMove,
  onDelete,
}: RowActionsMenuProps) {
  if (!row || (row.kind !== "entry" && row.kind !== "root")) {
    return <Menu anchorEl={anchor} open={false} onClose={onClose} />;
  }

  const act = (fn: (row: Row) => void) => () => {
    fn(row);
    onClose();
  };

  const folderish = canWriteInto(row);
  const isDirectory = row.kind === "root" || row.entry.kind === "dir";

  return (
    <Menu anchorEl={anchor} open={Boolean(anchor)} onClose={onClose}>
      {canDownload(row) && (
        <MenuItem onClick={act(onDownload)}>
          <ListItemIcon>
            <DownloadIcon fontSize="small" />
          </ListItemIcon>
          <ListItemText>Download</ListItemText>
        </MenuItem>
      )}
      {isDirectory && (
        <MenuItem disabled={!folderish} onClick={act(onUploadInto)}>
          <ListItemIcon>
            <UploadFileIcon fontSize="small" />
          </ListItemIcon>
          <ListItemText>Upload files here…</ListItemText>
        </MenuItem>
      )}
      {isDirectory && (
        <MenuItem disabled={!folderish} onClick={act(onNewFolder)}>
          <ListItemIcon>
            <CreateNewFolderIcon fontSize="small" />
          </ListItemIcon>
          <ListItemText>New folder…</ListItemText>
        </MenuItem>
      )}
      {isDirectory && (
        <MenuItem onClick={act(onRefresh)}>
          <ListItemIcon>
            <RefreshIcon fontSize="small" />
          </ListItemIcon>
          <ListItemText>Re-read this folder</ListItemText>
        </MenuItem>
      )}
      {row.kind === "entry" && <Divider />}
      {row.kind === "entry" && (
        <MenuItem disabled={!canRename(row)} onClick={act(onRename)}>
          <ListItemIcon>
            <DriveFileRenameOutlineIcon fontSize="small" />
          </ListItemIcon>
          <ListItemText>Rename</ListItemText>
        </MenuItem>
      )}
      {row.kind === "entry" && (
        <MenuItem disabled={!canRename(row)} onClick={act(onMove)}>
          <ListItemIcon>
            <DriveFileMoveIcon fontSize="small" />
          </ListItemIcon>
          {/* The drag is the accelerator; this is the interface. A phone has
              no drag worth the name, and a screen reader has no drop. */}
          <ListItemText>Move to…</ListItemText>
        </MenuItem>
      )}
      {row.kind === "entry" && (
        <MenuItem
          disabled={!canDelete(row)}
          onClick={act(onDelete)}
          sx={{ color: "error.main" }}
        >
          <ListItemIcon>
            <DeleteOutlineIcon fontSize="small" color="error" />
          </ListItemIcon>
          <ListItemText>Delete…</ListItemText>
        </MenuItem>
      )}
    </Menu>
  );
}
