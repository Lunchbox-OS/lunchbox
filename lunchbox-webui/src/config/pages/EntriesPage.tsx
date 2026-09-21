/**
 * Activities, as a board grouped by category.
 *
 * Dragging is how two genuinely spatial things are set here: which column a
 * card is in is `group = "..."`, and where it sits in that column is where its
 * `[[entries]]` block sits in the file, which is the order the launcher draws
 * the compartment in (issue #210). `@dnd-kit` rather than raw pointer events
 * because it brings keyboard and screen-reader support with it, which matters
 * more here than on the schedule grid, where every gesture already has a
 * numeric equivalent in the detail panel.
 */
import { Fragment, useEffect, useMemo, useState } from "react";
import {
  DndContext,
  DragOverlay,
  KeyboardSensor,
  PointerSensor,
  useDndContext,
  useDraggable,
  useSensor,
  useSensors,
  type DragEndEvent,
  type DragStartEvent,
} from "@dnd-kit/core";
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
import Link from "@mui/material/Link";
import Stack from "@mui/material/Stack";
import TextField from "@mui/material/TextField";
import Typography from "@mui/material/Typography";
import AddIcon from "@mui/icons-material/Add";
import CloseIcon from "@mui/icons-material/Close";
import DeleteIcon from "@mui/icons-material/DeleteOutlined";
import ErrorIcon from "@mui/icons-material/ErrorOutlined";
import DragIndicatorIcon from "@mui/icons-material/DragIndicator";
import { useConfigDoc } from "../doc/ConfigDocProvider";
import { dragKey, insert, unset } from "../doc/patches";
import type { RawConfig, RawEntry } from "../model/config.generated";
import { KIND_LABELS, blankKind, type KindTag } from "../model/kinds";
import { issuesForEntry } from "../model/report";
import { columnOf, entryDropPatches, UNGROUPED } from "../model/reorder";
import { EntryDetail } from "../components/EntryDetail";
import { DropGap, droppedGap, gapCollision, useDropColumn } from "../components/DropGap";
import type { FocusRequest } from "../navigation";

export function EntriesPage({
  config,
  focus,
  onFocusHandled,
  onOpenGroup,
}: {
  config: RawConfig;
  /** A request from elsewhere to open one activity's drawer. */
  focus?: FocusRequest | null;
  /** Called once the request has been acted on, so it cannot fire again. */
  onFocusHandled?: () => void;
  /** Jump to a category's own settings. */
  onOpenGroup?: (groupId: string) => void;
}) {
  const { apply, endGesture, report } = useConfigDoc();
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState<RawEntry | null>(null);
  const [draggingId, setDraggingId] = useState<string | null>(null);

  // Consumed on apply: this page unmounts when you leave the tab, and a fresh
  // mount runs this effect whatever its deps say, so an unspent request would
  // re-open the same activity every time you came back.
  useEffect(() => {
    if (!focus) return;
    setSelectedId(focus.subject.id);
    onFocusHandled?.();
  }, [focus?.nonce]); // eslint-disable-line react-hooks/exhaustive-deps

  const sensors = useSensors(useSensor(PointerSensor, {
    // A small threshold so clicking a card to open it still works.
    activationConstraint: { distance: 6 },
  }), useSensor(KeyboardSensor));

  const entries = config.entries ?? [];
  const groups = config.groups ?? [];
  const selected = entries.find((e) => e.id === selectedId) ?? null;

  // Categories first, in the order they are declared — the order the launcher
  // draws them in — then the leftovers, which is also where it puts them.
  const knownGroups = useMemo(() => new Set(groups.map((g) => g.id)), [groups]);
  const columns = useMemo(() => {
    const byGroup = new Map<string, RawEntry[]>();
    for (const g of groups) byGroup.set(g.id, []);
    byGroup.set(UNGROUPED, []);
    for (const entry of entries) byGroup.get(columnOf(entry, knownGroups))?.push(entry);
    return byGroup;
  }, [entries, groups, knownGroups]);

  const onDragStart = (event: DragStartEvent) => setDraggingId(String(event.active.id));

  const onDragEnd = (event: DragEndEvent) => {
    setDraggingId(null);
    const gap = droppedGap(event.over);
    if (!gap) return;
    const patches = entryDropPatches(
      entries,
      knownGroups,
      String(event.active.id),
      gap.column,
      gap.slot,
    );
    // Category and position are one gesture, so they are one undo step.
    for (const patch of patches) apply(patch, dragKey("entries.order"));
    endGesture();
  };

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

      <DndContext
        sensors={sensors}
        collisionDetection={gapCollision}
        onDragStart={onDragStart}
        onDragEnd={onDragEnd}
        onDragCancel={() => setDraggingId(null)}
        accessibility={{
          screenReaderInstructions: {
            draggable:
              "Press space to pick up an activity, then use the arrow keys to move it " +
              "between and within the category columns. Press space again to drop it " +
              "where it sits, or escape to cancel.",
          },
          announcements: {
            onDragStart: ({ active }) => `Picked up ${labelOf(entries, active.id)}.`,
            onDragOver: ({ active, over }) =>
              `${labelOf(entries, active.id)} would go ${describeGap(over, columns, groups)}.`,
            onDragEnd: ({ active, over }) =>
              over
                ? `${labelOf(entries, active.id)} dropped ${describeGap(over, columns, groups)}.`
                : `${labelOf(entries, active.id)} left where it was.`,
            onDragCancel: ({ active }) => `${labelOf(entries, active.id)} left where it was.`,
          },
        }}
      >
        <Stack
          direction="row"
          spacing={2}
          sx={{ overflowX: "auto", pb: 2, alignItems: "stretch" }}
        >
          {[...columns.entries()].map(([groupId, members]) => (
            <GroupColumn
              key={groupId}
              id={groupId}
              label={labelOfColumn(groupId, groups)}
              count={members.length}
              onOpen={groupId === UNGROUPED ? undefined : () => onOpenGroup?.(groupId)}
            >
              {members.map((entry, i) => (
                <Fragment key={entry.id}>
                  <DropGap column={groupId} slot={i} />
                  <EntryCard
                    entry={entry}
                    issueCount={issuesForEntry(report, entry.id).length}
                    selected={entry.id === selectedId}
                    onOpen={() => setSelectedId(entry.id)}
                    onDelete={() => setConfirmDelete(entry)}
                  />
                </Fragment>
              ))}
              <DropGap column={groupId} slot={members.length} grow />
            </GroupColumn>
          ))}
        </Stack>

        {/* Follows the pointer, at full opacity, above everything. */}
        <DragOverlay>
          {draggingId && (
            <Card variant="outlined" sx={{ cursor: "grabbing", boxShadow: 6 }}>
              <CardContent sx={{ p: 1.25, "&:last-child": { pb: 1.25 } }}>
                <Stack direction="row" spacing={1} sx={{ alignItems: "center" }}>
                  <DragIndicatorIcon fontSize="small" color="disabled" />
                  <Typography variant="body2" sx={{ fontWeight: 600 }} noWrap>
                    {labelOf(entries, draggingId)}
                  </Typography>
                </Stack>
              </CardContent>
            </Card>
          )}
        </DragOverlay>
      </DndContext>

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

/** How a column is named, wherever it is named. */
function labelOfColumn(groupId: string, groups: { id: string; label: string }[]): string {
  if (groupId === UNGROUPED) return "No category";
  return groups.find((g) => g.id === groupId)?.label ?? groupId;
}

function labelOf(entries: RawEntry[], id: string | number): string {
  return entries.find((e) => e.id === String(id))?.label ?? String(id);
}

/** What a drop on this gap would mean, for the screen-reader announcements. */
function describeGap(
  over: { id: string | number; data: { current?: unknown } } | null,
  columns: Map<string, RawEntry[]>,
  groups: { id: string; label: string }[],
): string {
  const gap = droppedGap(over);
  if (!gap) return "nowhere";
  const where = labelOfColumn(gap.column, groups);
  const above = columns.get(gap.column)?.[gap.slot];
  return above ? `into ${where}, above ${above.label}` : `at the end of ${where}`;
}

function GroupColumn({
  id,
  label,
  count,
  children,
  onOpen,
}: {
  id: string;
  label: string;
  count: number;
  children: React.ReactNode;
  /** Absent for the "No category" column, which has no settings to open. */
  onOpen?: () => void;
}) {
  // Registered only so `gapCollision` can tell which column a card is over;
  // nothing is ever dropped on the column itself.
  const { setNodeRef } = useDropColumn(id);
  const { over } = useDndContext();
  const isOver = (over?.data.current as { column?: string } | undefined)?.column === id;
  return (
    <Box
      ref={setNodeRef}
      sx={{
        minWidth: 260,
        maxWidth: 300,
        flexShrink: 0,
        display: "flex",
        flexDirection: "column",
        p: 1,
        borderRadius: 2,
        border: "1px dashed",
        borderColor: isOver ? "primary.main" : "divider",
        backgroundColor: isOver ? "action.hover" : "transparent",
      }}
    >
      <Stack direction="row" spacing={1} sx={{ alignItems: "center", mb: 1, px: 0.5 }}>
        {onOpen ? (
          <Link
            component="button"
            type="button"
            onClick={onOpen}
            underline="hover"
            variant="subtitle2"
            title={`Open ${label}'s schedule, limits and token gate`}
            sx={{ flex: 1, textAlign: "left", cursor: "pointer" }}
          >
            {label}
          </Link>
        ) : (
          <Typography variant="subtitle2" sx={{ flex: 1 }}>
            {label}
          </Typography>
        )}
        <Chip size="small" label={count} />
      </Stack>
      {/* No spacing: the drop gaps between the cards are the spacing, so the
          board does not shift when a drag begins. */}
      <Box sx={{ display: "flex", flexDirection: "column", flexGrow: 1, minHeight: 80 }}>
        {children}
      </Box>
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
  const { attributes, listeners, setNodeRef, isDragging } = useDraggable({ id: entry.id });

  // The card being dragged stays in its slot as a hole in the board, and a
  // copy of it follows the pointer in the `DragOverlay`. Translating this one
  // instead would slide a half-transparent card over the cards it passes,
  // which is exactly when it matters most to see where it is going.
  return (
    <Card
      ref={setNodeRef}
      variant="outlined"
      onClick={onOpen}
      sx={{
        cursor: "grab",
        opacity: isDragging ? 0.3 : entry.disabled ? 0.55 : 1,
        borderColor: selected ? "primary.main" : issueCount > 0 ? "error.main" : "divider",
        borderWidth: selected ? 2 : 1,
      }}
      {...attributes}
      {...listeners}
    >
      <CardContent sx={{ p: 1.25, "&:last-child": { pb: 1.25 } }}>
        <Stack direction="row" spacing={1} sx={{ alignItems: "flex-start" }}>
          {/* Says the card is draggable. The whole card is the handle, so this
              is decoration and never a second tab stop. */}
          <DragIndicatorIcon fontSize="small" color="disabled" sx={{ mt: 0.25 }} />
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
  const [kind, setKind] = useState<KindTag>("process");

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
    onAdd({
      id: effectiveId,
      label: label.trim(),
      kind: blankKind(kind),
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
            onChange={(e) => setKind(e.target.value as KindTag)}
          >
            {(Object.keys(KIND_LABELS) as KindTag[]).map((t) => (
              <MenuItem key={t} value={t}>
                {KIND_LABELS[t]}
              </MenuItem>
            ))}
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
