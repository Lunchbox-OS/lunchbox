/**
 * What an activity actually launches.
 *
 * `RawEntryKind` is an internally-tagged union with nine variants, so
 * switching the type rewrites the whole `kind` table. That is one `set` on
 * `kind` rather than a field-by-field migration: the shapes have almost nothing
 * in common, and carrying over `args`/`env` where they exist is enough.
 */
import FormControlLabel from "@mui/material/FormControlLabel";
import MenuItem from "@mui/material/MenuItem";
import Stack from "@mui/material/Stack";
import Switch from "@mui/material/Switch";
import TextField from "@mui/material/TextField";
import Typography from "@mui/material/Typography";
import type {
  EbookLayout,
  RawEntryKind,
  RawMediaMode,
  RawMediaQuality,
  RawMediaSortBy,
  RetroarchSaveState,
} from "../model/config.generated";
import { KeyValueEditor } from "./KeyValueEditor";
import { StringListEditor } from "./StringListEditor";
import { KIND_FIELD_DEFAULTS } from "../model/field-defaults.generated";

type KindTag = RawEntryKind["type"];

const KIND_LABELS: Record<KindTag, string> = {
  process: "Program",
  snap: "Snap",
  steam: "Steam game",
  flatpak: "Flatpak",
  vm: "Virtual machine",
  media: "Media library",
  retroarch: "Emulated game",
  ebook: "Book",
  custom: "Custom",
};

const KIND_HINTS: Record<KindTag, string> = {
  process: "Runs a command directly.",
  snap: "Launched through snap, with systemd scope-based process management.",
  steam: "Launched through the Steam snap by App ID.",
  flatpak: "Launched through flatpak by application ID.",
  vm: "Handed to a VM driver.",
  media: "Opens a shepherd-media library.",
  retroarch: "Boots one ROM or disc image through RetroArch.",
  ebook: "Opens one book in a reader locked to reading it.",
  custom: "Passed through to a host adapter that understands the type name.",
};

// Labelled as Records over the generated unions, not arrays: that is what
// turned #129's new `retroarch` variant into a compile error here rather than a
// silently missing menu entry, and the same holds if a quality or sort option
// is ever added or dropped.
const MEDIA_MODES: Record<RawMediaMode, string> = {
  browse: "Browse the library",
  play: "Play a single item",
};

const MEDIA_QUALITIES: Record<RawMediaQuality, string> = {
  best: "Best available",
  "1080p": "1080p",
  "720p": "720p",
  "480p": "480p",
};

const MEDIA_SORTS: Record<RawMediaSortBy, string> = {
  library: "Library order",
  title: "Title",
  id: "Id",
  kind: "Kind",
  category: "Category",
  duration: "Duration",
};

const EBOOK_LAYOUTS: Record<EbookLayout, string> = {
  facing_first_centered: "Two pages, cover on its own",
  facing: "Two pages, from the first",
  single: "One page at a time",
  scroll: "Scrolling column (for touch-only screens)",
};

const SAVE_STATES: Record<RetroarchSaveState, string> = {
  auto: "Resume where they stopped",
  off: "Boot fresh every time",
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
        onChange({
          ...base,
          command: "",
          args: args ?? [],
          env: env ?? {},
        } as RawEntryKind);
        break;
      case "snap":
        onChange({
          ...base,
          snap_name: "",
          args: args ?? [],
          env: env ?? {},
        } as RawEntryKind);
        break;
      case "steam":
        onChange({
          ...base,
          app_id: 0,
          args: args ?? [],
          env: env ?? {},
        } as RawEntryKind);
        break;
      case "flatpak":
        onChange({
          ...base,
          app_id: "",
          args: args ?? [],
          env: env ?? {},
        } as RawEntryKind);
        break;
      case "vm":
        onChange({ ...base, driver: "", args: {} } as RawEntryKind);
        break;
      case "media":
        onChange({ ...base, library: "" } as RawEntryKind);
        break;
      case "retroarch":
        onChange({
          ...base,
          content: "",
          core: "",
          args: args ?? [],
          env: env ?? {},
        } as RawEntryKind);
        break;
      case "ebook":
        onChange({ ...base, book: "", args: args ?? [], env: env ?? {} } as RawEntryKind);
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
        <>
          <TextField
            size="small"
            label="Library"
            required
            value={kind.library}
            onChange={(e) => patch({ library: e.target.value })}
            placeholder="~/Media/films.toml"
            helperText="A library .toml, .m3u/.m3u8, or a YouTube playlist URL."
          />
          <TextField
            select
            size="small"
            label="Opens"
            value={kind.mode ?? KIND_FIELD_DEFAULTS.media.mode}
            onChange={(e) => patch({ mode: e.target.value as RawMediaMode })}
          >
            {(Object.keys(MEDIA_MODES) as RawMediaMode[]).map((m) => (
              <MenuItem key={m} value={m}>
                {MEDIA_MODES[m]}
              </MenuItem>
            ))}
          </TextField>
          {/* `item` is required by, and only valid with, mode = "play". */}
          {(kind.mode ?? KIND_FIELD_DEFAULTS.media.mode) === "play" && (
            <TextField
              size="small"
              label="Item id"
              required
              value={kind.item ?? ""}
              onChange={(e) => patch({ item: e.target.value || null })}
              helperText="Which item in the library to play end to end."
            />
          )}
          <TextField
            select
            size="small"
            label="Maximum quality"
            value={kind.quality ?? "1080p"}
            onChange={(e) =>
              patch({ quality: e.target.value as RawMediaQuality })
            }
          >
            {(Object.keys(MEDIA_QUALITIES) as RawMediaQuality[]).map((q) => (
              <MenuItem key={q} value={q}>
                {MEDIA_QUALITIES[q]}
              </MenuItem>
            ))}
          </TextField>
          <TextField
            select
            size="small"
            label="Order items by"
            value={kind.sort_by ?? KIND_FIELD_DEFAULTS.media.sort_by}
            onChange={(e) =>
              patch({ sort_by: e.target.value as RawMediaSortBy })
            }
          >
            {(Object.keys(MEDIA_SORTS) as RawMediaSortBy[]).map((o) => (
              <MenuItem key={o} value={o}>
                {MEDIA_SORTS[o]}
              </MenuItem>
            ))}
          </TextField>
          <FormControlLabel
            control={
              <Switch
                checked={kind.reverse ?? KIND_FIELD_DEFAULTS.media.reverse}
                onChange={(e) => patch({ reverse: e.target.checked })}
              />
            }
            label="Reverse that order"
          />
          <FormControlLabel
            control={
              <Switch
                checked={kind.resume ?? KIND_FIELD_DEFAULTS.media.resume}
                onChange={(e) => patch({ resume: e.target.checked })}
              />
            }
            label="Remember playback positions"
          />
          <Typography variant="caption" color="text.secondary" sx={{ mt: -1 }}>
            Off by default: with it off, nothing about what was watched is
            written to disk.
          </Typography>
          {/* Tri-state, so not a Switch: null means "inherit
              service.media.prefetch", which is not the same as false. */}
          <TextField
            select
            size="small"
            label="Prefetch remote items"
            value={
              kind.prefetch == null ? "inherit" : kind.prefetch ? "on" : "off"
            }
            onChange={(e) =>
              patch({
                prefetch:
                  e.target.value === "inherit" ? null : e.target.value === "on",
              })
            }
            helperText="Download this library in the background."
          >
            <MenuItem value="inherit">Follow the service setting</MenuItem>
            <MenuItem value="on">Always</MenuItem>
            <MenuItem value="off">Never</MenuItem>
          </TextField>
          {/* Tri-state for the same reason: null inherits
              service.media.sponsorblock.enabled, which is not the same as
              false. Which categories to skip stays on the service table — the
              need here is "not in this library", e.g. a channel whose sponsor
              reads are part of the show. */}
          <TextField
            select
            size="small"
            label="Skip sponsors"
            value={
              kind.sponsorblock == null
                ? "inherit"
                : kind.sponsorblock
                  ? "on"
                  : "off"
            }
            onChange={(e) =>
              patch({
                sponsorblock:
                  e.target.value === "inherit" ? null : e.target.value === "on",
              })
            }
            helperText="Jump over sponsored spans in this library's YouTube videos."
          >
            <MenuItem value="inherit">Follow the service setting</MenuItem>
            <MenuItem value="on">Always</MenuItem>
            <MenuItem value="off">Never</MenuItem>
          </TextField>
        </>
      )}

      {kind.type === "retroarch" && (
        <>
          <TextField
            size="small"
            label="Content"
            required
            value={kind.content}
            onChange={(e) => patch({ content: e.target.value })}
            placeholder="~/Games/pokemon-firered.gba"
            helperText="The ROM or disc image. Absolute, or starting with ~/."
          />
          {/* Exactly one of core / core_path is required, so this is one
              choice with two spellings rather than two independent fields. */}
          <TextField
            select
            size="small"
            label="Core"
            value={kind.core_path != null ? "path" : "name"}
            onChange={(e) =>
              patch(
                e.target.value === "path"
                  ? { core: null, core_path: "" }
                  : { core: "", core_path: null },
              )
            }
          >
            <MenuItem value="name">By short name</MenuItem>
            <MenuItem value="path">By path</MenuItem>
          </TextField>
          {kind.core_path != null ? (
            <TextField
              size="small"
              label="Core path"
              required
              value={kind.core_path}
              onChange={(e) => patch({ core_path: e.target.value })}
              placeholder="/usr/lib/libretro/mgba_libretro.so"
            />
          ) : (
            <TextField
              size="small"
              label="Core name"
              required
              value={kind.core ?? ""}
              onChange={(e) => patch({ core: e.target.value })}
              placeholder="mgba"
              helperText="Resolved to e.g. mgba_libretro.so."
            />
          )}
          <TextField
            select
            size="small"
            label="On reopening"
            value={kind.save_state ?? KIND_FIELD_DEFAULTS.retroarch.save_state}
            onChange={(e) =>
              patch({ save_state: e.target.value as RetroarchSaveState })
            }
            helperText="The emulator's snapshot. The in-game save carries over either way."
          >
            {(Object.keys(SAVE_STATES) as RetroarchSaveState[]).map((v) => (
              <MenuItem key={v} value={v}>
                {SAVE_STATES[v]}
              </MenuItem>
            ))}
          </TextField>
          <FormControlLabel
            control={
              <Switch
                checked={kind.kiosk ?? KIND_FIELD_DEFAULTS.retroarch.kiosk}
                onChange={(e) => patch({ kiosk: e.target.checked })}
              />
            }
            label="Lock RetroArch's own menu"
          />
          <FormControlLabel
            control={
              <Switch
                checked={kind.reset ?? KIND_FIELD_DEFAULTS.retroarch.reset}
                onChange={(e) => patch({ reset: e.target.checked })}
              />
            }
            label="Offer the HUD's reset button"
          />
          <TextField
            size="small"
            label="RetroArch binary (optional)"
            value={kind.command ?? ""}
            onChange={(e) => patch({ command: e.target.value })}
            placeholder={KIND_FIELD_DEFAULTS.retroarch.command}
          />
        </>
      )}

      {kind.type === "ebook" && (
        <>
          <TextField
            size="small"
            label="Book"
            required
            value={kind.book}
            onChange={(e) => patch({ book: e.target.value })}
            placeholder="~/Books/the-hobbit.epub"
            helperText="EPUB, PDF, CBZ or DjVu. Absolute, or starting with ~/."
          />
          <TextField
            select
            size="small"
            label="Layout"
            value={kind.layout ?? KIND_FIELD_DEFAULTS.ebook.layout}
            onChange={(e) => patch({ layout: e.target.value as EbookLayout })}
            helperText="Facing pages suit a landscape screen, single a portrait one. A touch-only screen needs the scrolling column: there is no way to turn a page without a key, D-pad or wheel."
          >
            {(Object.keys(EBOOK_LAYOUTS) as EbookLayout[]).map((v) => (
              <MenuItem key={v} value={v}>
                {EBOOK_LAYOUTS[v]}
              </MenuItem>
            ))}
          </TextField>
          <TextField
            size="small"
            type="number"
            label="Text size"
            value={kind.font_size ?? KIND_FIELD_DEFAULTS.ebook.font_size}
            onChange={(e) => patch({ font_size: Number(e.target.value) })}
            helperText="Points, for a reflowed EPUB. Changing it repaginates the book, which moves a saved place -- set it before the first read."
          />
          <TextField
            size="small"
            label="Font"
            value={kind.font_family ?? ""}
            onChange={(e) => patch({ font_family: e.target.value })}
            placeholder={KIND_FIELD_DEFAULTS.ebook.font_family}
          />
          <TextField
            size="small"
            type="number"
            label="Open at page (optional)"
            value={kind.open_at ?? ""}
            onChange={(e) =>
              patch({ open_at: e.target.value === "" ? null : Number(e.target.value) })
            }
            helperText="First launch only; after that the reader reopens where it was left."
          />
          <FormControlLabel
            control={
              <Switch
                checked={kind.kiosk ?? KIND_FIELD_DEFAULTS.ebook.kiosk}
                onChange={(e) => patch({ kiosk: e.target.checked })}
              />
            }
            label="Lock the reader to this book"
          />
          <TextField
            size="small"
            label="Reader binary (optional)"
            value={kind.command ?? ""}
            onChange={(e) => patch({ command: e.target.value })}
            placeholder="okular"
          />
        </>
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

      {kind.type === "vm" && (
        <Typography variant="caption" color="text.secondary">
          Driver arguments are free-form and are edited in the raw TOML pane.
        </Typography>
      )}
    </Stack>
  );
}
