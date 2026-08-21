/**
 * Categories: a shared schedule and a combined daily budget.
 *
 * The combined quota is the point — once a category's budget is spent, every
 * member disappears at once — so the member list sits beside the limits rather
 * than on another page.
 */
import { useEffect, useState } from "react";
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
import { useConfigDoc } from "../doc/ConfigDocProvider";
import { insert, unset } from "../doc/patches";
import type { RawConfig, RawGroup } from "../model/config.generated";
import { issuesForGroup } from "../model/report";
import { SubjectDetail } from "../components/SubjectDetail";
import type { FocusRequest } from "../navigation";

export function GroupsPage({
  config,
  focus,
  onOpenEntry,
}: {
  config: RawConfig;
  /** A request from elsewhere to select one category. */
  focus?: FocusRequest | null;
  /** Jump to one of this category's members. */
  onOpenEntry?: (entryId: string) => void;
}) {
  const { apply, report } = useConfigDoc();
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState<RawGroup | null>(null);

  const groups = config.groups ?? [];
  const selected = groups.find((g) => g.id === selectedId) ?? groups[0] ?? null;

  const membersOf = (id: string) => (config.entries ?? []).filter((e) => e.group === id);

  // Keyed on the nonce so a repeat request still takes effect. See navigation.ts.
  useEffect(() => {
    if (focus) setSelectedId(focus.subject.id);
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
          <Stack spacing={1} sx={{ minWidth: 220 }}>
            {groups.map((g) => {
              const issues = issuesForGroup(report, g.id).length;
              return (
                <Card
                  key={g.id}
                  variant="outlined"
                  onClick={() => setSelectedId(g.id)}
                  sx={{
                    cursor: "pointer",
                    borderColor:
                      selected?.id === g.id
                        ? "primary.main"
                        : issues > 0
                          ? "error.main"
                          : "divider",
                    borderWidth: selected?.id === g.id ? 2 : 1,
                  }}
                >
                  <CardContent sx={{ p: 1.25, "&:last-child": { pb: 1.25 } }}>
                    <Stack direction="row" spacing={1} sx={{ alignItems: "center" }}>
                      <Box sx={{ flex: 1, minWidth: 0 }}>
                        <Typography variant="body2" sx={{ fontWeight: 600 }} noWrap>
                          {g.label}
                        </Typography>
                        <Typography variant="caption" color="text.secondary">
                          {membersOf(g.id).length} activities
                        </Typography>
                      </Box>
                      <IconButton
                        size="small"
                        aria-label={`Delete ${g.label}`}
                        onClick={(e) => {
                          e.stopPropagation();
                          setConfirmDelete(g);
                        }}
                      >
                        <DeleteIcon fontSize="small" />
                      </IconButton>
                    </Stack>
                  </CardContent>
                </Card>
              );
            })}
          </Stack>

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
