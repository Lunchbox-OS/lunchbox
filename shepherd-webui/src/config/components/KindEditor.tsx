/**
 * What an activity actually launches.
 *
 * `RawEntryKind` is an internally-tagged union with seven variants, so
 * switching the type rewrites the whole `kind` table. That is one `set` on
 * `kind` rather than a field-by-field migration: the shapes have almost nothing
 * in common, and carrying over `args`/`env` where they exist is enough.
 */
import MenuItem from "@mui/material/MenuItem";
import Stack from "@mui/material/Stack";
import TextField from "@mui/material/TextField";
import Typography from "@mui/material/Typography";
import type { RawEntryKind } from "../model/config.generated";
import { KeyValueEditor } from "./KeyValueEditor";
import { StringListEditor } from "./StringListEditor";

type KindTag = RawEntryKind["type"];

const KIND_LABELS: Record<KindTag, string> = {
  process: "Program",
  snap: "Snap",
  steam: "Steam game",
  flatpak: "Flatpak",
  vm: "Virtual machine",
  media: "Media library",
  custom: "Custom",
};

const KIND_HINTS: Record<KindTag, string> = {
  process: "Runs a command directly.",
  snap: "Launched through snap, with systemd scope-based process management.",
  steam: "Launched through the Steam snap by App ID.",
  flatpak: "Launched through flatpak by application ID.",
  vm: "Handed to a VM driver.",
  media: "Opens a shepherd-media library by id.",
  custom: "Passed through to a host adapter that understands the type name.",
};

interface Props {
  kind: RawEntryKind;
  onChange: (kind: RawEntryKind) => void;
}

export function KindEditor({ kind, onChange }: Props) {
  const patch = (fields: Record<string, unknown>) =>
    onChange({ ...kind, ...fields } as RawEntryKind);

  const switchTo = (type: KindTag) => {
    if (type === kind.type) return;
    // Carry across what the new shape can also hold; the rest has no analogue.
    const args = "args" in kind ? kind.args : undefined;
    const env = "env" in kind ? kind.env : undefined;
    const base = { type } as Record<string, unknown>;
    switch (type) {
      case "process":
        onChange({ ...base, command: "", args: args ?? [], env: env ?? {} } as RawEntryKind);
        break;
      case "snap":
        onChange({ ...base, snap_name: "", args: args ?? [], env: env ?? {} } as RawEntryKind);
        break;
      case "steam":
        onChange({ ...base, app_id: 0, args: args ?? [], env: env ?? {} } as RawEntryKind);
        break;
      case "flatpak":
        onChange({ ...base, app_id: "", args: args ?? [], env: env ?? {} } as RawEntryKind);
        break;
      case "vm":
        onChange({ ...base, driver: "", args: {} } as RawEntryKind);
        break;
      case "media":
        onChange({ ...base, library_id: "", args: {} } as RawEntryKind);
        break;
      case "custom":
        onChange({ ...base, type_name: "" } as RawEntryKind);
        break;
    }
  };

  return (
    <Stack spacing={2}>
      <TextField
        select
        size="small"
        label="Type"
        value={kind.type}
        onChange={(e) => switchTo(e.target.value as KindTag)}
        helperText={KIND_HINTS[kind.type]}
      >
        {(Object.keys(KIND_LABELS) as KindTag[]).map((t) => (
          <MenuItem key={t} value={t}>
            {KIND_LABELS[t]}
          </MenuItem>
        ))}
      </TextField>

      {kind.type === "process" && (
        <>
          <TextField
            size="small"
            label="Command"
            required
            value={kind.command}
            onChange={(e) => patch({ command: e.target.value })}
            placeholder="/usr/bin/tuxmath"
          />
          <TextField
            size="small"
            label="Working directory (optional)"
            value={kind.cwd ?? ""}
            onChange={(e) => patch({ cwd: e.target.value || null })}
          />
        </>
      )}

      {kind.type === "snap" && (
        <>
          <TextField
            size="small"
            label="Snap name"
            required
            value={kind.snap_name}
            onChange={(e) => patch({ snap_name: e.target.value })}
            placeholder="mc-installer"
          />
          <TextField
            size="small"
            label="Command (defaults to the snap name)"
            value={kind.command ?? ""}
            onChange={(e) => patch({ command: e.target.value || null })}
          />
        </>
      )}

      {kind.type === "steam" && (
        <TextField
          size="small"
          type="number"
          label="Steam App ID"
          required
          value={kind.app_id}
          onChange={(e) => patch({ app_id: Number(e.target.value) })}
          helperText="From the game's store URL, e.g. 504230 for Celeste."
        />
      )}

      {kind.type === "flatpak" && (
        <TextField
          size="small"
          label="Application ID"
          required
          value={kind.app_id}
          onChange={(e) => patch({ app_id: e.target.value })}
          placeholder="org.prismlauncher.PrismLauncher"
        />
      )}

      {kind.type === "vm" && (
        <TextField
          size="small"
          label="Driver"
          required
          value={kind.driver}
          onChange={(e) => patch({ driver: e.target.value })}
        />
      )}

      {kind.type === "media" && (
        <TextField
          size="small"
          label="Library id"
          required
          value={kind.library_id}
          onChange={(e) => patch({ library_id: e.target.value })}
          helperText="Matches library_id in the shepherd-media library file."
        />
      )}

      {kind.type === "custom" && (
        <TextField
          size="small"
          label="Type name"
          required
          value={kind.type_name}
          onChange={(e) => patch({ type_name: e.target.value })}
        />
      )}

      {"args" in kind && Array.isArray(kind.args) && (
        <StringListEditor
          label="Arguments"
          values={kind.args ?? []}
          onChange={(args) => patch({ args })}
          placeholder="-f"
        />
      )}

      {"env" in kind && kind.env !== undefined && (
        <KeyValueEditor
          label="Environment variables"
          values={kind.env ?? {}}
          onChange={(env) => patch({ env })}
        />
      )}

      {(kind.type === "vm" || kind.type === "media") && (
        <Typography variant="caption" color="text.secondary">
          Driver arguments are free-form and are edited in the raw TOML pane.
        </Typography>
      )}
    </Stack>
  );
}
