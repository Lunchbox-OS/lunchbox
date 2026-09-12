/**
 * Files (issue #195).
 *
 * One screen: the device's roots are the top level of a tree, and a folder is
 * opened in place rather than navigated into. There is no current directory,
 * no breadcrumb and no back button, which is what lets a device with a home
 * directory and two removable drives still be one view — and what lets a
 * parent see both ends of a copy at once.
 *
 * Reading and writing: upload (a button, the ⋮ menu, or a drop onto a folder),
 * new folder, rename in place, move (drag a row onto a folder, or "Move to…"
 * for anybody who cannot drag), delete. The architecture it follows is
 * `docs/ai/history/2026-09-11 005 remote-file-manager-ui (#195).md`.
 */
import { useCallback, useMemo, useRef, useState } from "react";
import Alert from "@mui/material/Alert";
import Box from "@mui/material/Box";
import Button from "@mui/material/Button";
import CircularProgress from "@mui/material/CircularProgress";
import FormControlLabel from "@mui/material/FormControlLabel";
import Paper from "@mui/material/Paper";
import Snackbar from "@mui/material/Snackbar";
import Stack from "@mui/material/Stack";
import Switch from "@mui/material/Switch";
import Typography from "@mui/material/Typography";
import CreateNewFolderIcon from "@mui/icons-material/CreateNewFolder";
import RefreshIcon from "@mui/icons-material/Refresh";
import UploadFileIcon from "@mui/icons-material/UploadFile";
import { useTheme } from "@mui/material/styles";
import useMediaQuery from "@mui/material/useMediaQuery";
import { downloadFile, DEFAULT_MAX_PAGES, LARGE_DOWNLOAD_BYTES } from "../api/files";
import { ApiError, isSameOriginApi } from "../api/client";
import { FileTreeTable } from "../files/FileTreeTable";
import { RowActionsMenu } from "../files/RowActionsMenu";
import { DeleteConfirmDialog, NewFolderDialog } from "../files/dialogs";
import { MoveToDialog } from "../files/MoveToDialog";
import { directoriesRefused, moveDestination, readDrop } from "../files/dnd";
import { canWriteInto } from "../files/permissions";
import {
  describe,
  useDirectories,
  useFileRoots,
  useFilesRefresh,
} from "../files/useDirectories";
import { basename, describeWriteFailure, useFileActions } from "../files/useFileActions";
import { useFileTree } from "../files/useFileTree";
import { busyDirectories, useUploads } from "../files/useUploads";
import {
  type NodeKey,
  type Row,
  buildRows,
  nodeKey,
  parentPath,
  reachableExpanded,
  splitKey,
} from "../files/tree";
import type { RootInfo } from "../files/types";

/** Where a write is aimed: a root, and a directory inside it. */
interface Target {
  rootId: string;
  dir: string;
  /** What to call it in a sentence. */
  label: string;
}

export function FilesPage() {
  const theme = useTheme();
  const compact = !useMediaQuery(theme.breakpoints.up("sm"));
  const tree = useFileTree();
  const refresh = useFilesRefresh();
  const roots = useFileRoots();
  const uploads = useUploads();
  const [renaming, setRenaming] = useState<NodeKey | null>(null);
  const [menu, setMenu] = useState<{ row: Row; anchor: HTMLElement } | null>(null);
  const [message, setMessage] = useState<{ text: string; error: boolean } | null>(null);
  const [newFolderIn, setNewFolderIn] = useState<Target | null>(null);
  const [deleting, setDeleting] = useState<Row | null>(null);
  const [movingRow, setMovingRow] = useState<Row | null>(null);
  const uploadTarget = useRef<Target | null>(null);
  const fileInput = useRef<HTMLInputElement>(null);

  const actions = useFileActions(tree.forgetSubtree);
  const rootList = useMemo(() => roots.data?.roots ?? [], [roots.data]);

  // Only the folders actually on screen are fetched: one whose parent is shut
  // stays remembered, so reopening the parent puts it back as it was, but
  // asking the device for it would be a request nobody can see the answer to.
  const openKeys = useMemo(
    () => reachableExpanded(tree.state.expanded, rootList),
    [tree.state.expanded, rootList],
  );
  const directories = useDirectories(openKeys, tree.state.pageLimits);

  const rows = useMemo(
    () =>
      buildRows({
        roots: rootList,
        expanded: tree.state.expanded,
        directories,
        sort: tree.state.sort,
        foldersFirst: tree.state.foldersFirst,
        showHidden: tree.state.showHidden,
      }),
    [rootList, directories, tree.state],
  );

  const busy = useMemo(() => busyDirectories(uploads.transfers), [uploads.transfers]);
  const selectedRow = useMemo(
    () => rows.find((row) => row.key === tree.state.selected) ?? null,
    [rows, tree.state.selected],
  );

  const rootOf = useCallback(
    (rootId: string): RootInfo | undefined => rootList.find((r) => r.id === rootId),
    [rootList],
  );

  /** Where a toolbar button aims: the selection if it can take files, else nothing. */
  const selectedTarget = useMemo((): Target | null => {
    if (!selectedRow || !canWriteInto(selectedRow)) return null;
    return targetOf(selectedRow);
  }, [selectedRow]);

  const say = (text: string, error = false) => setMessage({ text, error });

  // -- uploads --------------------------------------------------------------

  const send = useCallback(
    (target: Target, files: File[]) => {
      if (files.length === 0) return;
      const refused = uploads.start({
        rootId: target.rootId,
        dir: target.dir,
        files,
        root: rootOf(target.rootId),
        limits: roots.data?.limits,
      });
      // Refused before a byte left the browser: over the device's cap, or over
      // what is left on the disk.
      if (refused.length > 0) say(refused.join(" "), true);
      // Open the folder that is about to receive them, so the arrival is
      // somewhere the person can see.
      tree.expand(nodeKey(target.rootId, target.dir));
    },
    [uploads, rootOf, roots.data, tree],
  );

  const pickFiles = (target: Target) => {
    uploadTarget.current = target;
    fileInput.current?.click();
  };

  const onDropFiles = useCallback(
    (row: Row, transfer: DataTransfer) => {
      const { files, directories: folders } = readDrop(transfer);
      if (folders.length > 0) say(directoriesRefused(folders), true);
      if (files.length > 0) send(targetOf(row), files);
    },
    [send],
  );

  // -- the other writes -----------------------------------------------------

  const commitRename = useCallback(
    (row: Row, name: string) => {
      setRenaming(null);
      if (row.kind !== "entry") return;
      actions.rename.mutate(
        { rootId: row.rootId, path: row.path, name },
        {
          onError: (error) => say(describeWriteFailure(error, name), true),
        },
      );
    },
    [actions.rename],
  );

  /**
   * Move a row into a folder.
   *
   * A `409` is a question rather than a failure — something is already there,
   * and only a person can say whether this should win. Folders are not
   * offered the choice: the API will not replace one, and a recursive merge is
   * not a thing this does.
   */
  const doMove = useCallback(
    (source: Row, target: Row, overwrite = false) => {
      if (source.kind !== "entry") return;
      const { rootId, dir } = moveDestination(target);
      actions.move.mutate(
        { rootId, from: source.path, toDir: dir, overwrite },
        {
          onSuccess: () => {
            setMovingRow(null);
            // Open the destination, so the thing that moved is where the
            // person can see it landed.
            tree.expand(nodeKey(rootId, dir));
            say(`${basename(source.path)} moved.`);
          },
          onError: (error) => {
            const clash =
              error instanceof ApiError &&
              error.code === "conflict" &&
              source.entry.kind === "file";
            if (clash && window.confirm(
              `Something called ${source.entry.name} is already in that folder. Replace it?`,
            )) {
              doMove(source, target, true);
              return;
            }
            say(describeWriteFailure(error, basename(source.path)), true);
          },
        },
      );
    },
    [actions.move, tree],
  );

  const confirmDelete = () => {
    const row = deleting;
    if (!row || row.kind !== "entry") return;
    actions.remove.mutate(
      {
        rootId: row.rootId,
        path: row.path,
        // A folder has no version to match, so it goes with `If-Match: *`; a
        // file is pinned to the one this list was drawn from.
        etag: row.entry.kind === "dir" ? null : row.entry.etag,
        recursive: row.entry.kind === "dir",
      },
      {
        onSuccess: () => {
          setDeleting(null);
          say(`${row.entry.name} deleted.`);
        },
        onError: (error) => say(describeWriteFailure(error, row.entry.name), true),
      },
    );
  };

  async function download(row: Row) {
    if (row.kind !== "entry") return;
    const size = row.entry.size ?? 0;
    if (!isSameOriginApi() && size > LARGE_DOWNLOAD_BYTES) {
      // The cross-device path buffers the whole file in memory, so this is a
      // real question rather than a pedantic one.
      const proceed = window.confirm(
        `${row.entry.name} is large, and downloading it from another device holds ` +
          `the whole file in this browser's memory. Download anyway?`,
      );
      if (!proceed) return;
    }
    try {
      await downloadFile(row.rootId, row.path, row.entry.name);
    } catch (error) {
      say(describe(error), true);
    }
  }

  if (roots.isPending) {
    return (
      <Box sx={{ display: "flex", justifyContent: "center", p: 6 }}>
        <CircularProgress />
      </Box>
    );
  }

  if (roots.isError) {
    return (
      <Alert
        severity="error"
        action={
          <Button color="inherit" size="small" onClick={() => roots.refetch()}>
            Try again
          </Button>
        }
      >
        {describe(roots.error)}
      </Alert>
    );
  }

  return (
    <Stack spacing={2}>
      <Box
        sx={{
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          gap: 2,
          flexWrap: "wrap",
        }}
      >
        <Typography variant="h6">Files</Typography>
        <Stack direction="row" spacing={1} sx={{ alignItems: "center", flexWrap: "wrap" }}>
          <Button
            size="small"
            startIcon={<UploadFileIcon />}
            disabled={!selectedTarget}
            onClick={() => selectedTarget && pickFiles(selectedTarget)}
          >
            Upload
          </Button>
          <Button
            size="small"
            startIcon={<CreateNewFolderIcon />}
            disabled={!selectedTarget}
            onClick={() => setNewFolderIn(selectedTarget)}
          >
            New folder
          </Button>
          <FormControlLabel
            control={
              <Switch
                size="small"
                checked={tree.state.showHidden}
                onChange={(e) => tree.setShowHidden(e.target.checked)}
              />
            }
            label={<Typography variant="body2">Show hidden</Typography>}
          />
          <Button size="small" startIcon={<RefreshIcon />} onClick={() => refresh.all()}>
            Refresh
          </Button>
        </Stack>
      </Box>

      <Typography variant="body2" color="text.secondary">
        {selectedTarget
          ? `Uploads and new folders go into ${selectedTarget.label}. Files can also be dropped straight onto a folder.`
          : "Choose a folder to upload into, or drop files straight onto one. A book, a ROM or a video has to be on the device before an activity can use it."}
      </Typography>

      {rootList.length === 0 ? (
        <Paper variant="outlined" sx={{ p: 3 }}>
          <Typography variant="body2" color="text.secondary">
            No places to browse. Removable drives appear here when they are
            mounted; more can be added with
            <code> [[service.file_manager.extra_roots]] </code>
            in the config.
          </Typography>
        </Paper>
      ) : (
        <FileTreeTable
          rows={rows}
          compact={compact}
          selected={tree.state.selected}
          renaming={renaming}
          busy={busy}
          sort={tree.state.sort}
          onSort={tree.sortBy}
          onSelect={tree.select}
          onToggle={tree.toggle}
          onActivate={(row) => {
            if (row.kind === "root") tree.toggle(row.key);
            else if (row.kind === "entry" && row.entry.kind === "dir") {
              tree.toggle(row.key);
            } else if (row.kind === "entry") {
              void download(row);
            }
          }}
          onMenu={(row, anchor) => setMenu({ row, anchor })}
          onRetry={(node) => refresh.directory(node)}
          onShowMore={(node: NodeKey) =>
            tree.showMore(
              node,
              (tree.state.pageLimits.get(node) ?? DEFAULT_MAX_PAGES) + DEFAULT_MAX_PAGES,
            )
          }
          onRenameCommit={commitRename}
          onRenameCancel={() => setRenaming(null)}
          onDropFiles={onDropFiles}
          onMove={(source, target) => doMove(source, target)}
          onMoveRefused={(reason) => say(reason, true)}
          onExpand={tree.expand}
        />
      )}

      <RowActionsMenu
        row={menu?.row ?? null}
        anchor={menu?.anchor ?? null}
        onClose={() => setMenu(null)}
        onDownload={(row) => void download(row)}
        onRefresh={(row) => {
          const { rootId } = splitKey(row.key);
          const path = row.kind === "entry" ? row.path : "";
          void refresh.directory(nodeKey(rootId, path));
        }}
        onUploadInto={(row) => pickFiles(targetOf(row))}
        onNewFolder={(row) => setNewFolderIn(targetOf(row))}
        onRename={(row) => {
          tree.select(row.key);
          setRenaming(row.key);
        }}
        onMove={(row) => setMovingRow(row)}
        onDelete={(row) => setDeleting(row)}
      />

      <NewFolderDialog
        open={newFolderIn !== null}
        where={newFolderIn?.label ?? ""}
        busy={actions.newFolder.isPending}
        error={
          actions.newFolder.error
            ? describeWriteFailure(actions.newFolder.error, "that folder")
            : null
        }
        onCancel={() => {
          setNewFolderIn(null);
          actions.newFolder.reset();
        }}
        onCreate={(name) => {
          if (!newFolderIn) return;
          actions.newFolder.mutate(
            { rootId: newFolderIn.rootId, parent: newFolderIn.dir, name },
            {
              onSuccess: () => {
                tree.expand(nodeKey(newFolderIn.rootId, newFolderIn.dir));
                setNewFolderIn(null);
                actions.newFolder.reset();
              },
            },
          );
        }}
      />

      <MoveToDialog
        open={movingRow !== null}
        source={movingRow}
        roots={rootList}
        busy={actions.move.isPending}
        error={
          actions.move.error
            ? describeWriteFailure(actions.move.error, "that")
            : null
        }
        onCancel={() => {
          setMovingRow(null);
          actions.move.reset();
        }}
        onMove={(target) => movingRow && doMove(movingRow, target)}
      />

      <DeleteConfirmDialog
        open={deleting !== null}
        name={deleting?.kind === "entry" ? deleting.entry.name : ""}
        isFolder={deleting?.kind === "entry" && deleting.entry.kind === "dir"}
        busy={actions.remove.isPending}
        error={
          actions.remove.error
            ? describeWriteFailure(actions.remove.error, "that")
            : null
        }
        onCancel={() => {
          setDeleting(null);
          actions.remove.reset();
        }}
        onDelete={confirmDelete}
      />

      {/* One input for every upload path: the toolbar, the ⋮ menu, and a
          keyboard user who cannot drag. */}
      <input
        ref={fileInput}
        type="file"
        multiple
        hidden
        onChange={(e) => {
          const target = uploadTarget.current;
          const files = Array.from(e.target.files ?? []);
          if (target) send(target, files);
          // So picking the same file twice in a row still fires a change.
          e.target.value = "";
        }}
      />

      <Snackbar
        open={Boolean(message)}
        autoHideDuration={6000}
        onClose={() => setMessage(null)}
      >
        <Alert
          severity={message?.error ? "error" : "success"}
          onClose={() => setMessage(null)}
        >
          {message?.text}
        </Alert>
      </Snackbar>
    </Stack>
  );
}

/** Where writes aimed at this row should land. */
function targetOf(row: Row): Target {
  if (row.kind === "root") {
    return { rootId: row.root.id, dir: "", label: row.root.label };
  }
  if (row.kind === "entry" && row.entry.kind === "dir") {
    return { rootId: row.rootId, dir: row.path, label: row.entry.name };
  }
  // A file: its folder, which is what "upload here" means next to one.
  const { rootId, path } = splitKey(row.key);
  const dir = parentPath(path);
  return { rootId, dir, label: dir === "" ? "that place" : dir };
}
