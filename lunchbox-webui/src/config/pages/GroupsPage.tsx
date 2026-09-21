/**
 * Categories: a shared schedule and a combined daily budget.
 *
 * The combined quota is the point — once a category's budget is spent, every
 * member disappears at once — so the member list sits beside the limits rather
 * than on another page.
 *
 * The list is also the home screen's running order: the launcher draws one
 * compartment per category in the order they are declared (issue #210), so
 * dragging one up this list moves its compartment up the screen.
 */
import { Fragment, useEffect, useState } from "react";
import {
  DndContext,
  DragOverlay,
  KeyboardSensor,
  PointerSensor,
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
import IconButton from "@mui/material/IconButton";
import Stack from "@mui/material/Stack";
import TextField from "@mui/material/TextField";
import Typography from "@mui/material/Typography";
import AddIcon from "@mui/icons-material/Add";
import DeleteIcon from "@mui/icons-material/DeleteOutlined";
import DragIndicatorIcon from "@mui/icons-material/DragIndicator";
import { useConfigDoc } from "../doc/ConfigDocProvider";
import { insert, unset } from "../doc/patches";
import type { RawConfig, RawGroup } from "../model/config.generated";
import { issuesForGroup } from "../model/report";
import { groupDropPatch } from "../model/reorder";
import { SubjectDetail } from "../components/SubjectDetail";
import { DropGap, droppedGap, gapCollision, useDropColumn } from "../components/DropGap";
import type { FocusRequest } from "../navigation";

/** The one column this page's drag happens in. */
const CATEGORIES = "categories";

export function GroupsPage({
  config,
  focus,
  onFocusHandled,
  onOpenEntry,
}: {
  config: RawConfig;
  /** A request from elsewhere to select one category. */
  focus?: FocusRequest | null;
  /** Called once the request has been acted on, so it cannot fire again. */
  onFocusHandled?: () => void;
  /** Jump to one of this category's members. */
  onOpenEntry?: (entryId: string) => void;
}) {
  const { apply, report } = useConfigDoc();
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState<RawGroup | null>(null);
  const [draggingId, setDraggingId] = useState<string | null>(null);

  const groups = config.groups ?? [];
  const selected = groups.find((g) => g.id === selectedId) ?? groups[0] ?? null;

  const membersOf = (id: string) => (config.entries ?? []).filter((e) => e.group === id);

  const sensors = useSensors(
    // A small threshold so a click still selects the category it lands on.
    useSensor(PointerSensor, { activationConstraint: { distance: 6 } }),
    useSensor(KeyboardSensor),
  );

  const onDragStart = (event: DragStartEvent) => setDraggingId(String(event.active.id));

  const onDragEnd = (event: DragEndEvent) => {
    setDraggingId(null);
    const gap = droppedGap(event.over);
    if (!gap) return;
    const patch = groupDropPatch(groups, String(event.active.id), gap.slot);
    if (patch) apply(patch);
  };

  const nameOf = (id: string | number) =>
    groups.find((g) => g.id === String(id))?.label ?? String(id);
  const describe = (over: { data: { current?: unknown } } | null) => {
    const gap = droppedGap(over);
    if (!gap) return "nowhere";
    const below = groups[gap.slot];
    return below ? `above ${below.label}` : "at the end";
  };

  // Consumed on apply — see navigation.ts for why an unspent request re-fires.
  useEffect(() => {
    if (!focus) return;
    setSelectedId(focus.subject.id);
    onFocusHandled?.();
  }, [focus?.nonce]); // eslint-disable-line react-hooks/exhaustive-deps

  return (
    <Box>
      <Stack direction="row" spacing={2} sx={{ alignItems: "center", mb: 2 }}>
        <Typography variant="h6" sx={{ flex: 1 }}>
          Categories
        </Typography>
        <Button variant="contained" startIcon={<AddIcon />} onClick={() => setAdding(true)}>
          Add category
        </Button>
      </Stack>

      {groups.length === 0 ? (
        <Card variant="outlined">
          <CardContent>
            <Typography variant="body2" color="text.secondary">
              No categories. They are optional — add one when several activities should
              share a schedule, or draw from one combined daily budget.
            </Typography>
          </CardContent>
        </Card>
      ) : (
        <Stack direction={{ xs: "column", md: "row" }} spacing={2}>
          <DndContext
            sensors={sensors}
            collisionDetection={gapCollision}
            onDragStart={onDragStart}
            onDragEnd={onDragEnd}
            onDragCancel={() => setDraggingId(null)}
            accessibility={{
              screenReaderInstructions: {
                draggable:
                  "Press space to pick up a category, then use the up and down arrow " +
                  "keys to move it. Press space again to drop it, or escape to cancel. " +
                  "The order here is the order the home screen shows them in.",
              },
              announcements: {
                onDragStart: ({ active }) => `Picked up ${nameOf(active.id)}.`,
                onDragOver: ({ active, over }) =>
                  `${nameOf(active.id)} would go ${describe(over)}.`,
                onDragEnd: ({ active, over }) =>
                  over
                    ? `${nameOf(active.id)} dropped ${describe(over)}.`
                    : `${nameOf(active.id)} left where it was.`,
                onDragCancel: ({ active }) => `${nameOf(active.id)} left where it was.`,
              },
            }}
          >
            <CategoryList>
              {groups.map((g, i) => (
                <Fragment key={g.id}>
                  <DropGap column={CATEGORIES} slot={i} />
                  <CategoryCard
                    group={g}
                    memberCount={membersOf(g.id).length}
                    issueCount={issuesForGroup(report, g.id).length}
                    selected={selected?.id === g.id}
                    onSelect={() => setSelectedId(g.id)}
                    onDelete={() => setConfirmDelete(g)}
                  />
                </Fragment>
              ))}
              <DropGap column={CATEGORIES} slot={groups.length} grow />
            </CategoryList>

            {/* Follows the pointer, at full opacity, above everything. */}
            <DragOverlay>
              {draggingId && (
                <Card variant="outlined" sx={{ cursor: "grabbing", boxShadow: 6 }}>
                  <CardContent sx={{ p: 1.25, "&:last-child": { pb: 1.25 } }}>
                    <Stack direction="row" spacing={1} sx={{ alignItems: "center" }}>
                      <DragIndicatorIcon fontSize="small" color="disabled" />
                      <Typography variant="body2" sx={{ fontWeight: 600 }} noWrap>
                        {nameOf(draggingId)}
                      </Typography>
                    </Stack>
                  </CardContent>
                </Card>
              )}
            </DragOverlay>
          </DndContext>

          <Box sx={{ flex: 1, minWidth: 0 }}>
            {selected && (
              <GroupDetail
                key={selected.id}
                group={selected}
                members={membersOf(selected.id)}
                config={config}
                onOpenEntry={onOpenEntry}
              />
            )}
          </Box>
        </Stack>
      )}

      <AddGroupDialog
        open={adding}
        existingIds={groups.map((g) => g.id)}
        onClose={() => setAdding(false)}
        onAdd={(group) => {
          apply(insert("groups", group as never));
          setAdding(false);
          setSelectedId(group.id as string);
        }}
      />

      <Dialog open={confirmDelete !== null} onClose={() => setConfirmDelete(null)}>
        <DialogTitle>Delete “{confirmDelete?.label}”?</DialogTitle>
        <DialogContent>
          <Typography variant="body2">
            {confirmDelete && membersOf(confirmDelete.id).length > 0 ? (
              <>
                {membersOf(confirmDelete.id).length} activities point at this category and
                would be left referring to a category that no longer exists — which the
                validator will flag. Move them first, or fix them afterwards.
              </>
            ) : (
              <>Nothing points at this category, so removing it is safe.</>
            )}
          </Typography>
        </DialogContent>
        <DialogActions>
          <Button onClick={() => setConfirmDelete(null)}>Cancel</Button>
          <Button
            color="error"
            variant="contained"
            onClick={() => {
              if (!confirmDelete) return;
              apply(unset(`groups[id=${confirmDelete.id}]`));
              if (selectedId === confirmDelete.id) setSelectedId(null);
              setConfirmDelete(null);
            }}
          >
            Delete
          </Button>
        </DialogActions>
      </Dialog>
    </Box>
  );
}

/**
 * The column the category cards are dragged in. Registered as a drop column so
 * `gapCollision` has a rect to scope to, and given a minimum height so the
 * trailing gap is reachable even when there is only one category.
 */
function CategoryList({ children }: { children: React.ReactNode }) {
  const { setNodeRef } = useDropColumn(CATEGORIES);
  return (
    <Box
      ref={setNodeRef}
      sx={{
        minWidth: 220,
        display: "flex",
        flexDirection: "column",
        minHeight: 120,
      }}
    >
      {children}
    </Box>
  );
}

function CategoryCard({
  group,
  memberCount,
  issueCount,
  selected,
  onSelect,
  onDelete,
}: {
  group: RawGroup;
  memberCount: number;
  issueCount: number;
  selected: boolean;
  onSelect: () => void;
  onDelete: () => void;
}) {
  const { attributes, listeners, setNodeRef, isDragging } = useDraggable({ id: group.id });

  // Stays in its slot as a hole in the list while a copy follows the pointer
  // in the `DragOverlay`.
  return (
    <Card
      ref={setNodeRef}
      variant="outlined"
      onClick={onSelect}
      sx={{
        cursor: "grab",
        opacity: isDragging ? 0.3 : 1,
        borderColor: selected ? "primary.main" : issueCount > 0 ? "error.main" : "divider",
        borderWidth: selected ? 2 : 1,
      }}
      {...attributes}
      {...listeners}
    >
      <CardContent sx={{ p: 1.25, "&:last-child": { pb: 1.25 } }}>
        <Stack direction="row" spacing={1} sx={{ alignItems: "center" }}>
          {/* Decoration: the whole card is the handle, so this is never a
              second tab stop. */}
          <DragIndicatorIcon fontSize="small" color="disabled" />
          <Box sx={{ flex: 1, minWidth: 0 }}>
            <Typography variant="body2" sx={{ fontWeight: 600 }} noWrap>
              {group.label}
            </Typography>
            <Typography variant="caption" color="text.secondary">
              {memberCount} activities
            </Typography>
          </Box>
          <IconButton
            size="small"
            aria-label={`Delete ${group.label}`}
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

function GroupDetail({
  group,
  members,
  config,
  onOpenEntry,
}: {
  group: RawGroup;
  members: { id: string; label: string }[];
  config: RawConfig;
  onOpenEntry?: (entryId: string) => void;
}) {
  return (
    <SubjectDetail
      subject={{ kind: "group", id: group.id }}
      config={config}
      basics={
        <Box>
          <Typography variant="subtitle2" sx={{ mb: 1 }}>
            Members
          </Typography>
          {members.length === 0 ? (
            <Typography variant="body2" color="text.secondary">
              No activities in this category yet — drag cards into it on the Activities
              page.
            </Typography>
          ) : (
            <Stack direction="row" spacing={1} sx={{ flexWrap: "wrap" }} useFlexGap>
              {members.map((m) => (
                <Chip
                  key={m.id}
                  label={m.label}
                  size="small"
                  clickable={onOpenEntry !== undefined}
                  onClick={onOpenEntry ? () => onOpenEntry(m.id) : undefined}
                />
              ))}
            </Stack>
          )}
        </Box>
      }
    />
  );
}

function AddGroupDialog({
  open,
  existingIds,
  onClose,
  onAdd,
}: {
  open: boolean;
  existingIds: string[];
  onClose: () => void;
  onAdd: (group: Record<string, unknown>) => void;
}) {
  const [label, setLabel] = useState("");
  const [id, setId] = useState("");
  const [idTouched, setIdTouched] = useState(false);

  const suggested = label
    .toLowerCase()
    .trim()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
  const effectiveId = idTouched ? id : suggested;
  const duplicate = existingIds.includes(effectiveId);

  return (
    <Dialog open={open} onClose={onClose} fullWidth maxWidth="xs">
      <DialogTitle>Add a category</DialogTitle>
      <DialogContent>
        <Stack spacing={2} sx={{ mt: 1 }}>
          <TextField
            autoFocus
            size="small"
            label="Label"
            value={label}
            onChange={(e) => setLabel(e.target.value)}
          />
          <TextField
            size="small"
            label="Id"
            value={effectiveId}
            error={duplicate}
            helperText={duplicate ? "Another category already uses this id." : " "}
            onChange={(e) => {
              setIdTouched(true);
              setId(e.target.value);
            }}
          />
        </Stack>
      </DialogContent>
      <DialogActions>
        <Button onClick={onClose}>Cancel</Button>
        <Button
          variant="contained"
          disabled={!label.trim() || !effectiveId || duplicate}
          onClick={() => {
            onAdd({ id: effectiveId, label: label.trim() });
            setLabel("");
            setId("");
            setIdTouched(false);
          }}
        >
          Add
        </Button>
      </DialogActions>
    </Dialog>
  );
}
