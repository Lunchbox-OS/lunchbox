/**
 * Activities, as a board grouped by category.
 *
 * The category a card sits in comes from its `group` field, which is set from
 * the detail drawer for now.
 */
import { useMemo, useState } from "react";
import Box from "@mui/material/Box";
import Button from "@mui/material/Button";
import Card from "@mui/material/Card";
import CardContent from "@mui/material/CardContent";
import Chip from "@mui/material/Chip";
import Dialog from "@mui/material/Dialog";
import DialogActions from "@mui/material/DialogActions";
import DialogContent from "@mui/material/DialogContent";
import DialogTitle from "@mui/material/DialogTitle";
import Drawer from "@mui/material/Drawer";
import IconButton from "@mui/material/IconButton";
import MenuItem from "@mui/material/MenuItem";
import Stack from "@mui/material/Stack";
import TextField from "@mui/material/TextField";
import Typography from "@mui/material/Typography";
import AddIcon from "@mui/icons-material/Add";
import CloseIcon from "@mui/icons-material/Close";
import DeleteIcon from "@mui/icons-material/DeleteOutlined";
import ErrorIcon from "@mui/icons-material/ErrorOutlined";
import { useConfigDoc } from "../doc/ConfigDocProvider";
import { insert, unset } from "../doc/patches";
import type { RawConfig, RawEntry } from "../model/config.generated";
import { issuesForEntry } from "../model/report";
import { EntryDetail } from "../components/EntryDetail";

const UNGROUPED = "__ungrouped__";

export function EntriesPage({ config }: { config: RawConfig }) {
  const { apply, report } = useConfigDoc();
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState<RawEntry | null>(null);

  const entries = config.entries ?? [];
  const groups = config.groups ?? [];
  const selected = entries.find((e) => e.id === selectedId) ?? null;

  const columns = useMemo(() => {
    const byGroup = new Map<string, RawEntry[]>();
    byGroup.set(UNGROUPED, []);
    for (const g of groups) byGroup.set(g.id, []);
    for (const entry of entries) {
      const key = entry.group && byGroup.has(entry.group) ? entry.group : UNGROUPED;
      byGroup.get(key)?.push(entry);
    }
    return byGroup;
  }, [entries, groups]);

  const deleteEntry = (entry: RawEntry) => {
    apply(unset(`entries[id=${entry.id}]`));
    if (selectedId === entry.id) setSelectedId(null);
    setConfirmDelete(null);
  };

  return (
    <Box>
      <Stack direction="row" spacing={2} sx={{ alignItems: "center", mb: 2 }}>
        <Typography variant="h6" sx={{ flex: 1 }}>
          Activities
        </Typography>
        <Button variant="contained" startIcon={<AddIcon />} onClick={() => setAdding(true)}>
          Add activity
        </Button>
      </Stack>

      {entries.length === 0 && (
        <Card variant="outlined">
          <CardContent>
            <Typography variant="body2" color="text.secondary">
              No activities yet. Add one to get started — a config with no entries is valid
              but shows an empty launcher.
            </Typography>
          </CardContent>
        </Card>
      )}

      <Stack direction="row" spacing={2} sx={{ overflowX: "auto", pb: 2 }}>
        {[...columns.entries()].map(([groupId, members]) => (
          <GroupColumn
            key={groupId}
            label={
                groupId === UNGROUPED
                ? "No category"
                : (groups.find((g) => g.id === groupId)?.label ?? groupId)
            }
            count={members.length}
          >
            {members.map((entry) => (
              <EntryCard
                key={entry.id}
                entry={entry}
                issueCount={issuesForEntry(report, entry.id).length}
                selected={entry.id === selectedId}
                onOpen={() => setSelectedId(entry.id)}
                onDelete={() => setConfirmDelete(entry)}
              />
            ))}
          </GroupColumn>
        ))}
      </Stack>

      <Drawer
        anchor="right"
        open={selected !== null}
        onClose={() => setSelectedId(null)}
        slotProps={{ paper: { sx: { width: { xs: "100%", md: 720 }, p: 2 } } }}
      >
        {selected && (
          <>
            <Stack direction="row" sx={{ alignItems: "center", mb: 2 }}>
              <Typography variant="h6" sx={{ flex: 1 }}>
                {selected.label}
              </Typography>
              <Typography variant="caption" color="text.secondary" sx={{ mr: 1 }}>
                {selected.id}
              </Typography>
              <IconButton onClick={() => setSelectedId(null)} aria-label="Close">
                <CloseIcon />
              </IconButton>
            </Stack>
            <EntryDetail entry={selected} config={config} />
          </>
        )}
      </Drawer>

      <AddEntryDialog
        open={adding}
        existingIds={entries.map((e) => e.id)}
        groups={groups.map((g) => ({ id: g.id, label: g.label }))}
        onClose={() => setAdding(false)}
        onAdd={(entry) => {
          apply(insert("entries", entry as never));
          setAdding(false);
          setSelectedId(entry.id as string);
        }}
      />

      <Dialog open={confirmDelete !== null} onClose={() => setConfirmDelete(null)}>
        <DialogTitle>Delete “{confirmDelete?.label}”?</DialogTitle>
        <DialogContent>
          <Typography variant="body2">
            This removes the activity and its settings from the file. Everything else,
            including your comments, stays exactly as it is.
          </Typography>
        </DialogContent>
        <DialogActions>
          <Button onClick={() => setConfirmDelete(null)}>Cancel</Button>
          <Button
            color="error"
            variant="contained"
            onClick={() => confirmDelete && deleteEntry(confirmDelete)}
          >
            Delete
          </Button>
        </DialogActions>
      </Dialog>
    </Box>
  );
}

function GroupColumn({
  label,
  count,
  children,
}: {
  label: string;
  count: number;
  children: React.ReactNode;
}) {
  return (
    <Box
      sx={{
        minWidth: 260,
        maxWidth: 300,
        flexShrink: 0,
        p: 1,
        borderRadius: 2,
        border: "1px dashed",
        borderColor: "divider",
      }}
    >
      <Stack direction="row" spacing={1} sx={{ alignItems: "center", mb: 1, px: 0.5 }}>
        <Typography variant="subtitle2" sx={{ flex: 1 }}>
          {label}
        </Typography>
        <Chip size="small" label={count} />
      </Stack>
      <Stack spacing={1}>{children}</Stack>
    </Box>
  );
}

function EntryCard({
  entry,
  issueCount,
  selected,
  onOpen,
  onDelete,
}: {
  entry: RawEntry;
  issueCount: number;
  selected: boolean;
  onOpen: () => void;
  onDelete: () => void;
}) {
  return (
    <Card
      variant="outlined"
      onClick={onOpen}
      sx={{
        cursor: "pointer",
        opacity: entry.disabled ? 0.55 : 1,
        borderColor: selected ? "primary.main" : issueCount > 0 ? "error.main" : "divider",
        borderWidth: selected ? 2 : 1,
      }}
    >
      <CardContent sx={{ p: 1.25, "&:last-child": { pb: 1.25 } }}>
        <Stack direction="row" spacing={1} sx={{ alignItems: "flex-start" }}>
          <Box sx={{ flex: 1, minWidth: 0 }}>
            <Typography variant="body2" sx={{ fontWeight: 600 }} noWrap>
              {entry.label}
            </Typography>
            <Typography variant="caption" color="text.secondary" noWrap sx={{ display: "block" }}>
              {entry.kind.type}
              {entry.disabled ? " · disabled" : ""}
            </Typography>
          </Box>
          {issueCount > 0 && <ErrorIcon color="error" fontSize="small" />}
          <IconButton
            size="small"
            aria-label={`Delete ${entry.label}`}
            onClick={(e) => {
              e.stopPropagation();
              onDelete();
            }}
          >
            <DeleteIcon fontSize="small" />
          </IconButton>
        </Stack>
      </CardContent>
    </Card>
  );
}

function AddEntryDialog({
  open,
  existingIds,
  groups,
  onClose,
  onAdd,
}: {
  open: boolean;
  existingIds: string[];
  groups: { id: string; label: string }[];
  onClose: () => void;
  onAdd: (entry: Record<string, unknown>) => void;
}) {
  const [label, setLabel] = useState("");
  const [id, setId] = useState("");
  const [group, setGroup] = useState("");
  const [kind, setKind] = useState("process");

  // Suggest an id from the label until the id is edited by hand.
  const [idTouched, setIdTouched] = useState(false);
  const suggestedId = slugify(label);
  const effectiveId = idTouched ? id : suggestedId;
  const duplicate = existingIds.includes(effectiveId);

  const reset = () => {
    setLabel("");
    setId("");
    setGroup("");
    setKind("process");
    setIdTouched(false);
  };

  const submit = () => {
    const kindTable: Record<string, unknown> =
      kind === "process"
        ? { type: "process", command: "" }
        : kind === "snap"
          ? { type: "snap", snap_name: "" }
          : kind === "flatpak"
            ? { type: "flatpak", app_id: "" }
            : kind === "steam"
              ? { type: "steam", app_id: 0 }
              : { type: "media", library_id: "" };

    onAdd({
      id: effectiveId,
      label: label.trim(),
      kind: kindTable,
      ...(group ? { group } : {}),
    });
    reset();
  };

  return (
    <Dialog open={open} onClose={onClose} fullWidth maxWidth="sm">
      <DialogTitle>Add an activity</DialogTitle>
      <DialogContent>
        <Stack spacing={2} sx={{ mt: 1 }}>
          <TextField
            autoFocus
            size="small"
            label="Label"
            value={label}
            onChange={(e) => setLabel(e.target.value)}
            helperText="What the child sees."
          />
          <TextField
            size="small"
            label="Id"
            value={effectiveId}
            onChange={(e) => {
              setIdTouched(true);
              setId(e.target.value);
            }}
            error={duplicate}
            helperText={
              duplicate
                ? "Another activity already uses this id."
                : "Stable identifier, used in usage records and overrides. Hard to change later."
            }
          />
          <TextField
            select
            size="small"
            label="Type"
            value={kind}
            onChange={(e) => setKind(e.target.value)}
          >
            <MenuItem value="process">Program</MenuItem>
            <MenuItem value="snap">Snap</MenuItem>
            <MenuItem value="flatpak">Flatpak</MenuItem>
            <MenuItem value="steam">Steam game</MenuItem>
            <MenuItem value="media">Media library</MenuItem>
          </TextField>
          {groups.length > 0 && (
            <TextField
              select
              size="small"
              label="Category"
              value={group}
              onChange={(e) => setGroup(e.target.value)}
            >
              <MenuItem value="">
                <em>None</em>
              </MenuItem>
              {groups.map((g) => (
                <MenuItem key={g.id} value={g.id}>
                  {g.label}
                </MenuItem>
              ))}
            </TextField>
          )}
        </Stack>
      </DialogContent>
      <DialogActions>
        <Button onClick={onClose}>Cancel</Button>
        <Button
          variant="contained"
          disabled={!label.trim() || !effectiveId || duplicate}
          onClick={submit}
        >
          Add
        </Button>
      </DialogActions>
    </Dialog>
  );
}

function slugify(text: string): string {
  return text
    .toLowerCase()
    .trim()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
}
