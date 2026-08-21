/**
 * Volume and brightness restrictions.
 *
 * Both appear twice in the schema — once under `[service]` as a device default
 * and once per entry as an override — with the same shape, so both take a path.
 * Automatic brightness is the exception: it is a device-global mode, and a copy
 * under a per-entry override is ignored by the daemon, so it is only offered at
 * service level.
 */
import Slider from "@mui/material/Slider";
import Stack from "@mui/material/Stack";
import Switch from "@mui/material/Switch";
import FormControlLabel from "@mui/material/FormControlLabel";
import TextField from "@mui/material/TextField";
import Typography from "@mui/material/Typography";
import { useFields } from "../doc/useFields";
import type {
  RawAutoBrightnessConfig,
  RawBrightnessConfig,
  RawVolumeConfig,
} from "../model/config.generated";
import { Section } from "./Section";

function PercentRange({
  label,
  min,
  max,
  onChange,
  onCommit,
}: {
  label: string;
  min: number;
  max: number;
  onChange: (min: number, max: number) => void;
  onCommit: () => void;
}) {
  return (
    <Stack spacing={0.5}>
      <Typography variant="body2">{label}</Typography>
      <Slider
        value={[min, max]}
        min={0}
        max={100}
        size="small"
        valueLabelDisplay="auto"
        valueLabelFormat={(v) => `${v}%`}
        onChange={(_, next) => {
          const [lo, hi] = next as number[];
          onChange(lo, hi);
        }}
        onChangeCommitted={onCommit}
        getAriaLabel={(i) => (i === 0 ? `${label} minimum` : `${label} maximum`)}
      />
      <Typography variant="caption" color="text.secondary">
        {min}% – {max}%
      </Typography>
    </Stack>
  );
}

export function VolumeEditor({ path, value }: { path: string; value: RawVolumeConfig | null | undefined }) {
  const f = useFields(path);
  const present = value != null;

  return (
    <Section
      title="Volume"
      description="What the volume keys are allowed to do."
      present={present}
      onTogglePresent={(on) =>
        on ? f.setTable("", { allow_change: true, allow_mute: true }) : f.setField("", undefined)
      }
    >
      <Stack spacing={2}>
        <PercentRange
          label="Allowed range"
          min={value?.min_volume ?? 0}
          max={value?.max_volume ?? 100}
          onChange={(lo, hi) => {
            f.dragField("min_volume", lo);
            f.dragField("max_volume", hi);
          }}
          onCommit={f.commit}
        />
        <FormControlLabel
          control={
            <Switch
              checked={value?.allow_change ?? true}
              onChange={(e) => f.setField("allow_change", e.target.checked)}
            />
          }
          label="Allow volume changes at all"
        />
        <FormControlLabel
          control={
            <Switch
              checked={value?.allow_mute ?? true}
              onChange={(e) => f.setField("allow_mute", e.target.checked)}
            />
          }
          label="Allow mute"
        />
      </Stack>
    </Section>
  );
}

export function BrightnessEditor({
  path,
  value,
  allowAuto,
}: {
  path: string;
  value: RawBrightnessConfig | null | undefined;
  /** Automatic brightness is device-global; only offer it under [service]. */
  allowAuto?: boolean;
}) {
  const f = useFields(path);
  const auto = useFields(`${path}.auto`);
  const present = value != null;

  return (
    <Section
      title="Screen brightness"
      description="A minimum above zero stops the screen going dark enough to look broken."
      present={present}
      onTogglePresent={(on) =>
        on ? f.setTable("", { allow_change: true, min_brightness: 10 }) : f.setField("", undefined)
      }
    >
      <Stack spacing={2}>
        <PercentRange
          label="Allowed range"
          min={value?.min_brightness ?? 0}
          max={value?.max_brightness ?? 100}
          onChange={(lo, hi) => {
            f.dragField("min_brightness", lo);
            f.dragField("max_brightness", hi);
          }}
          onCommit={f.commit}
        />
        <FormControlLabel
          control={
            <Switch
              checked={value?.allow_change ?? true}
              onChange={(e) => f.setField("allow_change", e.target.checked)}
            />
          }
          label="Allow brightness changes"
        />

        {allowAuto && (
          <AutoBrightnessEditor value={value?.auto} fields={auto} present={value?.auto != null} />
        )}
      </Stack>
    </Section>
  );
}

function AutoBrightnessEditor({
  value,
  fields,
  present,
}: {
  value: RawAutoBrightnessConfig | null | undefined;
  fields: ReturnType<typeof useFields>;
  present: boolean;
}) {
  return (
    <Section
      title="Automatic (ambient light)"
      description="Only honoured under [service.brightness]; a per-activity copy is ignored."
      present={present}
      onTogglePresent={(on) =>
        on ? fields.setTable("", { enabled: false }) : fields.setField("", undefined)
      }
    >
      <Stack spacing={2}>
        <FormControlLabel
          control={
            <Switch
              checked={value?.enabled ?? false}
              onChange={(e) => fields.setField("enabled", e.target.checked)}
            />
          }
          label="Starts enabled"
        />
        <Typography variant="caption" color="text.secondary">
          Only the starting state — the runtime toggle from the HUD is persisted and wins
          once it has been used.
        </Typography>
        <Stack direction="row" spacing={2}>
          <TextField
            size="small"
            type="number"
            label="Dim below (lux)"
            value={value?.dim_lux ?? ""}
            onChange={(e) =>
              fields.setField("dim_lux", e.target.value === "" ? undefined : Number(e.target.value))
            }
          />
          <TextField
            size="small"
            type="number"
            label="Bright above (lux)"
            value={value?.bright_lux ?? ""}
            onChange={(e) =>
              fields.setField(
                "bright_lux",
                e.target.value === "" ? undefined : Number(e.target.value),
              )
            }
          />
        </Stack>
        <Stack direction="row" spacing={2}>
          <TextField
            size="small"
            type="number"
            label="Dim end (%)"
            value={value?.min_percent ?? ""}
            onChange={(e) =>
              fields.setField(
                "min_percent",
                e.target.value === "" ? undefined : Number(e.target.value),
              )
            }
          />
          <TextField
            size="small"
            type="number"
            label="Bright end (%)"
            value={value?.max_percent ?? ""}
            onChange={(e) =>
              fields.setField(
                "max_percent",
                e.target.value === "" ? undefined : Number(e.target.value),
              )
            }
          />
          <TextField
            size="small"
            type="number"
            label="Sample every (s)"
            value={value?.poll_interval_seconds ?? ""}
            onChange={(e) =>
              fields.setField(
                "poll_interval_seconds",
                e.target.value === "" ? undefined : Number(e.target.value),
              )
            }
          />
        </Stack>
      </Stack>
    </Section>
  );
}
