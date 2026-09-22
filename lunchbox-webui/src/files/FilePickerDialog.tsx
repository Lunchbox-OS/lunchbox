/**
 * "Choose a ROM…" — the file tree as a question (issue #186).
 *
 * The same shape as `MoveToDialog`, and for the same reasons: its own
 * expansion state so that looking for a book does not rearrange the Files tab
 * behind it, the same per-directory queries so a folder already open costs
 * nothing to show, and a refusal written under the button rather than a row
 * that quietly will not be clicked.
 *
 * What it adds over "move to…" is that the answer is usually a *file*, which
 * brings in everything the destination picker could ignore: a folder that
 * loaded only the first few thousand of its ROMs, a name that is not text, and
 * dotfiles — `~/.config/lunchbox/movies.toml` and `~/.config/retroarch/cores`
 * are both things a policy points at.
 *
 * It is mounted per-pick rather than kept alive, so every "Browse…" opens
 * where the field points rather than where the last one was left.
 */
import { useEffect, useMemo, useRef, useState } from "react";
import Alert from "@mui/material/Alert";
import Box from "@mui/material/Box";
import Button from "@mui/material/Button";
import CircularProgress from "@mui/material/CircularProgress";
import Dialog from "@mui/material/Dialog";
import DialogActions from "@mui/material/DialogActions";
import DialogContent from "@mui/material/DialogContent";
import DialogTitle from "@mui/material/DialogTitle";
import FormControlLabel from "@mui/material/FormControlLabel";
import IconButton from "@mui/material/IconButton";
import Link from "@mui/material/Link";
import Switch from "@mui/material/Switch";
import Tooltip from "@mui/material/Tooltip";
import Typography from "@mui/material/Typography";
import ChevronRightIcon from "@mui/icons-material/ChevronRight";
import ExpandMoreIcon from "@mui/icons-material/ExpandMore";
import FolderIcon from "@mui/icons-material/Folder";
import HomeIcon from "@mui/icons-material/Home";
import InsertDriveFileIcon from "@mui/icons-material/InsertDriveFile";
import PlaceIcon from "@mui/icons-material/Place";
import UploadFileIcon from "@mui/icons-material/UploadFile";
import UsbIcon from "@mui/icons-material/Usb";
import type { PickRequest } from "../config/pick/FilePicker";
import { DEFAULT_MAX_PAGES } from "../api/files";
import { canWriteInto } from "./permissions";
import { basename } from "./useFileActions";
import { ancestorKeys, configPathOf, locate, pickRefusal } from "./pick";
import {
  buildRows,
  isExpandable,
  joinPath,
  nodeKey,
  parentPath,
  reachableExpanded,
  type Row,
} from "./tree";
import { describe, useDirectories, useFileRoots, useFilesRefresh } from "./useDirectories";
import { useFileTree } from "./useFileTree";
import { useUploads } from "./useUploads";

export interface FilePickerDialogProps {
  request: PickRequest;
  onCancel: () => void;
  onChoose: (value: string) => void;
}

/** Where an upload started from here would land. */
interface UploadTarget {
  rootId: string;
  dir: string;
  /** What to call it in "Upload to Books…". */
  label: string;
}

export function FilePickerDialog({ request, onCancel, onChoose }: FilePickerDialogProps) {
  const roots = useFileRoots();
  const refresh = useFilesRefresh();
  const uploads = useUploads();
  const tree = useFileTree({ showHidden: request.showHidden ?? false });
  const fileInput = useRef<HTMLInputElement>(null);
  const [refused, setRefused] = useState<string | null>(null);
  // A single uploaded file to select once the device has it, or null.
  const arrival = useRef<{ rootId: string; dir: string; name: string } | null>(null);

  const rootList = useMemo(() => roots.data?.roots ?? [], [roots.data]);

  // Where the field points today, if it points anywhere on this device.
  const start = useMemo(
    () => (request.start ? locate(request.start, rootList) : null),
    [request.start, rootList],
  );

  // Open it, once, as soon as there are roots to open. Guarded by a ref
  // rather than by a dependency list: `tree` changes identity on every
  // keystroke of state, and re-running this would fight anybody who collapsed
  // what it opened.
  const opened = useRef(false);
  useEffect(() => {
    if (opened.current || rootList.length === 0) return;
    opened.current = true;
    if (!start) {
      // No useful starting point: open the home root, which is where almost
      // everything a policy names lives.
      const home = rootList.find((r) => r.kind === "home") ?? rootList[0];
      if (home) tree.expand(nodeKey(home.id, ""));
      return;
    }
    for (const key of ancestorKeys(start)) tree.expand(key);
    if (start.path !== "") {
      const key = nodeKey(start.rootId, start.path);
      tree.select(key);
      // A field that already names a folder is asking about what is in it, so
      // open that too. Not done for a file, which cannot be opened at all.
      if (request.kind === "directory") tree.expand(key);
    }
    // A path through a dotted directory is only reachable with these on, and
    // arriving at a dialog that does not show what the field says would be a
    // puzzle.
    if (start.path.split("/").some((part) => part.startsWith("."))) {
      tree.setShowHidden(true);
    }
  }, [rootList, start, tree, request.kind]);

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
        sort: { column: "name", direction: "asc" },
        foldersFirst: true,
        showHidden: tree.state.showHidden,
        // A folder is the only possible answer, so the files are noise.
        foldersOnly: request.kind === "directory",
      }),
    [rootList, directories, tree.state, request.kind],
  );

  const selected = rows.find((row) => row.key === tree.state.selected) ?? null;
  const refusal = pickRefusal(selected, request.kind);
  const value = selected && !refusal ? configPathOf(selected, rootList) : null;

  const choose = () => {
    if (value !== null) onChoose(value);
  };

  // -- putting the file there in the first place ----------------------------

  // Where an upload would land: the selected folder, or the folder holding
  // the selected file — which is what somebody who has just been told "that
  // ROM is not there" actually has selected.
  const target = useMemo((): UploadTarget | null => {
    if (!selected) return null;
    if (selected.kind === "root") {
      return selected.root.writable
        ? { rootId: selected.root.id, dir: "", label: selected.root.label }
        : null;
    }
    if (selected.kind !== "entry") return null;
    if (canWriteInto(selected)) {
      return { rootId: selected.rootId, dir: selected.path, label: selected.entry.name };
    }
    if (!selected.parentWritable) return null;
    const dir = parentPath(selected.path);
    const root = rootList.find((r) => r.id === selected.rootId);
    return {
      rootId: selected.rootId,
      dir,
      label: dir === "" ? (root?.label ?? "this place") : basename(dir),
    };
  }, [selected, rootList]);

  const send = (files: File[]) => {
    if (!target || files.length === 0) return;
    const refused = uploads.start({
      rootId: target.rootId,
      dir: target.dir,
      files,
      root: rootList.find((r) => r.id === target.rootId),
      limits: roots.data?.limits,
    });
    setRefused(refused.length > 0 ? refused.join(" ") : null);
    tree.expand(nodeKey(target.rootId, target.dir));
    // One file is an answer to the question on screen, so it is worth
    // selecting when it lands. Several are a stocking-up, and choosing one of
    // them on somebody's behalf would be a guess.
    const only = files.length - refused.length === 1 ? files[0] : undefined;
    arrival.current =
      only && !refused.some((r) => r.startsWith(only.name))
        ? { rootId: target.rootId, dir: target.dir, name: only.name }
        : null;
  };

  // A transfer is not the file: the device assembles it under `.name.part`
  // and publishes it with one rename at the end, so until it is `done` there
  // is nothing on the device for a policy to point at.
  useEffect(() => {
    const waiting = arrival.current;
    if (!waiting) return;
    const landed = uploads.transfers.some(
      (t) =>
        t.status === "done" &&
        t.rootId === waiting.rootId &&
        t.dir === waiting.dir &&
        t.name === waiting.name,
    );
    if (!landed) return;
    arrival.current = null;
    tree.select(nodeKey(waiting.rootId, joinPath(waiting.dir, waiting.name)));
  }, [uploads.transfers, tree]);

  return (
    <Dialog open onClose={onCancel} fullWidth maxWidth="sm">
      <DialogTitle>Choose {request.what}</DialogTitle>
      <DialogContent dividers sx={{ minHeight: 320 }}>
        {refused && (
          <Alert severity="warning" sx={{ mb: 2 }} onClose={() => setRefused(null)}>
            {refused}
          </Alert>
        )}
        {roots.isPending && (
          <Box sx={{ display: "flex", alignItems: "center", gap: 1 }}>
            <CircularProgress size={16} />
            <Typography variant="body2" color="text.secondary">
              Asking the device what it has…
            </Typography>
          </Box>
        )}
        {roots.isError && (
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
        )}
        {roots.isSuccess && (
          <Box role="tree" aria-label={`Choose ${request.what}`}>
            {rows.map((row) => (
              <PickerRow
                key={row.key}
                row={row}
                selected={row.key === tree.state.selected}
                refused={pickRefusal(row, request.kind) !== null}
                onSelect={() => tree.select(row.key)}
                onActivate={() => {
                  // Double-click does the obvious thing for what it is on: a
                  // folder opens, a valid answer is the answer.
                  if (row.kind === "entry" && isExpandable(row.entry)) {
                    tree.toggle(row.key);
                  } else if (pickRefusal(row, request.kind) === null) {
                    const picked = configPathOf(row, rootList);
                    if (picked !== null) onChoose(picked);
                  }
                }}
                onToggle={() => tree.toggle(row.key)}
                onRetry={() => refresh.directory(row.kind === "error" ? row.node : row.key)}
                onShowMore={() => {
                  if (row.kind !== "more") return;
                  tree.showMore(
                    row.node,
                    (tree.state.pageLimits.get(row.node) ?? DEFAULT_MAX_PAGES) +
                      DEFAULT_MAX_PAGES,
                  );
                }}
              />
            ))}
          </Box>
        )}
      </DialogContent>
      {/* The file may not be on the device yet, which is the other half of
          why a policy points at something that is not there. The queue is the
          Files tab's — the editor renders inside `UploadsProvider` — so this
          transfer survives closing the dialog and the tray shows it. */}
      <input
        ref={fileInput}
        type="file"
        multiple
        hidden
        onChange={(e) => {
          send(Array.from(e.target.files ?? []));
          // So picking the same file twice in a row still fires a change.
          e.target.value = "";
        }}
      />

      <DialogActions sx={{ justifyContent: "space-between", flexWrap: "wrap", gap: 1 }}>
        <Box sx={{ display: "flex", alignItems: "center", gap: 1, pl: 1 }}>
          <FormControlLabel
            control={
              <Switch
                size="small"
                checked={tree.state.showHidden}
                onChange={(e) => tree.setShowHidden(e.target.checked)}
              />
            }
            label={<Typography variant="body2">Hidden files</Typography>}
          />
          <Tooltip
            title={
              target
                ? `Put a file into ${target.label} from this computer`
                : "Choose a folder to put it in first"
            }
          >
            <span>
              <Button
                size="small"
                startIcon={<UploadFileIcon />}
                disabled={!target}
                onClick={() => fileInput.current?.click()}
              >
                Upload
              </Button>
            </span>
          </Tooltip>
        </Box>
        <Box sx={{ display: "flex", alignItems: "center", gap: 1 }}>
          <Typography variant="caption" color="text.secondary">
            {refusal ?? ""}
          </Typography>
          <Button onClick={onCancel}>Cancel</Button>
          <Button variant="contained" disabled={value === null} onClick={choose}>
            Use this
          </Button>
        </Box>
      </DialogActions>
    </Dialog>
  );
}

function PickerRow({
  row,
  selected,
  refused,
  onSelect,
  onActivate,
  onToggle,
  onRetry,
  onShowMore,
}: {
  row: Row;
  selected: boolean;
  refused: boolean;
  onSelect: () => void;
  onActivate: () => void;
  onToggle: () => void;
  onRetry: () => void;
  onShowMore: () => void;
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
          Nothing in here
        </Typography>
      </Indented>
    );
  }
  if (row.kind === "error") {
    return (
      <Indented depth={row.depth}>
        <Typography variant="body2" color="text.secondary">
          {row.message}{" "}
          <Link component="button" variant="body2" onClick={onRetry}>
            Try again
          </Link>
        </Typography>
      </Indented>
    );
  }
  if (row.kind === "more") {
    return (
      <Indented depth={row.depth}>
        <Typography variant="body2" color="text.secondary">
          Showing the first {row.shown}.{" "}
          <Link component="button" variant="body2" onClick={onShowMore}>
            Show more
          </Link>
        </Typography>
      </Indented>
    );
  }

  const isRoot = row.kind === "root";
  const name = isRoot ? row.root.label : row.entry.name;
  const expandable = isRoot || isExpandable(row.entry);
  const Icon = isRoot
    ? row.root.kind === "home"
      ? HomeIcon
      : row.root.kind === "external"
        ? UsbIcon
        : PlaceIcon
    : row.entry.kind === "dir"
      ? FolderIcon
      : InsertDriveFileIcon;

  return (
    <Box
      role="treeitem"
      aria-level={row.depth + 1}
      aria-selected={selected}
      aria-expanded={expandable ? row.expanded : undefined}
      onClick={onSelect}
      onDoubleClick={onActivate}
      sx={{
        display: "flex",
        alignItems: "center",
        gap: 0.5,
        pl: `${row.depth * 18}px`,
        py: 0.25,
        borderRadius: 1,
        cursor: "pointer",
        // Dimmed rather than hidden: a row that cannot be the answer is still
        // worth seeing, and clicking it says why.
        opacity: refused ? 0.55 : 1,
        bgcolor: selected ? "action.selected" : undefined,
        "&:hover": { bgcolor: selected ? "action.selected" : "action.hover" },
      }}
    >
      <IconButton
        size="small"
        disabled={!expandable}
        aria-label={row.expanded ? `Collapse ${name}` : `Expand ${name}`}
        onClick={(e) => {
          e.stopPropagation();
          onToggle();
        }}
        // Kept in the layout when it does nothing, so names line up.
        sx={{ visibility: expandable ? "visible" : "hidden" }}
      >
        {row.expanded ? (
          <ExpandMoreIcon fontSize="small" />
        ) : (
          <ChevronRightIcon fontSize="small" />
        )}
      </IconButton>
      <Icon
        fontSize="small"
        sx={{ color: !isRoot && row.entry.kind === "dir" ? "primary.light" : undefined }}
      />
      <Typography
        variant="body2"
        noWrap
        sx={{ fontWeight: isRoot ? 600 : 400, fontStyle: !isRoot && row.entry.hidden ? "italic" : undefined }}
      >
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
