/**
 * Files (issue #195).
 *
 * One screen: the device's roots are the top level of a tree, and a folder is
 * opened in place rather than navigated into. There is no current directory,
 * no breadcrumb and no back button, which is what lets a device with a home
 * directory and two removable drives still be one view — and what lets a
 * parent see both ends of a copy at once.
 *
 * This is the read-only half. Upload, rename, move and delete land next; the
 * architecture they follow is
 * `docs/ai/history/2026-09-11 005 remote-file-manager-ui (#195).md`.
 */
import { useMemo, useState } from "react";
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
import RefreshIcon from "@mui/icons-material/Refresh";
import { useTheme } from "@mui/material/styles";
import useMediaQuery from "@mui/material/useMediaQuery";
import { downloadFile, DEFAULT_MAX_PAGES, LARGE_DOWNLOAD_BYTES } from "../api/files";
import { isSameOriginApi } from "../api/client";
import { FileTreeTable } from "../files/FileTreeTable";
import { RowActionsMenu } from "../files/RowActionsMenu";
import {
  describe,
  useDirectories,
  useFileRoots,
  useFilesRefresh,
} from "../files/useDirectories";
import { useFileTree } from "../files/useFileTree";
import {
  type NodeKey,
  type Row,
  buildRows,
  nodeKey,
  reachableExpanded,
  splitKey,
} from "../files/tree";

export function FilesPage() {
  const theme = useTheme();
  const compact = !useMediaQuery(theme.breakpoints.up("sm"));
  const tree = useFileTree();
  const refresh = useFilesRefresh();
  const roots = useFileRoots();
  const [menu, setMenu] = useState<{ row: Row; anchor: HTMLElement } | null>(null);
  const [message, setMessage] = useState<string | null>(null);

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
      setMessage(describe(error));
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
        <Stack direction="row" spacing={1} sx={{ alignItems: "center" }}>
          <FormControlLabel
            control={
              <Switch
                size="small"
                checked={tree.state.showHidden}
                onChange={(e) => tree.setShowHidden(e.target.checked)}
              />
            }
            label={
              <Typography variant="body2">Show hidden</Typography>
            }
          />
          <Button
            size="small"
            startIcon={<RefreshIcon />}
            onClick={() => refresh.all()}
          >
            Refresh
          </Button>
        </Stack>
      </Box>

      <Typography variant="body2" color="text.secondary">
        This device's own files. Open a place to see what is in it — a book, a
        ROM or a video has to be on the device before an activity can use it.
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
              (tree.state.pageLimits.get(node) ?? DEFAULT_MAX_PAGES) +
                DEFAULT_MAX_PAGES,
            )
          }
        />
      )}

      <RowActionsMenu
        row={menu?.row ?? null}
        anchor={menu?.anchor ?? null}
        onClose={() => setMenu(null)}
        onDownload={(row) => void download(row)}
        onRefresh={(row) => {
          if (row.kind !== "entry") return;
          const { rootId } = splitKey(row.key);
          void refresh.directory(nodeKey(rootId, row.path));
        }}
      />

      <Snackbar
        open={Boolean(message)}
        autoHideDuration={6000}
        onClose={() => setMessage(null)}
      >
        <Alert severity="error" onClose={() => setMessage(null)}>
          {message}
        </Alert>
      </Snackbar>
    </Stack>
  );
}
