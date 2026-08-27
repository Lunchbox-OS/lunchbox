/**
 * A budget, as a slider.
 *
 * Two things make this more than a wrapper around `<Slider>`:
 *
 * - **The scale is not linear.** The useful range runs from a minute to about
 *   eight hours, and a linear track spends most of its length on values nobody
 *   picks. Steps are one minute up to an hour and five minutes above it.
 * - **Zero means unlimited.** `seconds_to_duration_or_unlimited` in
 *   `policy.rs` treats 0 as "no limit", so a slider that quietly bottomed out
 *   at zero would set the opposite of what it looked like. The bottom of the
 *   track is an explicit detent, labelled.
 *
 * Inherited values — the service default, or the group's — are drawn as marks
 * on the track, so it is visible what happens if this one is cleared.
 */
import Box from "@mui/material/Box";
import Slider from "@mui/material/Slider";
import Stack from "@mui/material/Stack";
import Tooltip from "@mui/material/Tooltip";
import Typography from "@mui/material/Typography";
import { formatDurationHuman } from "../../shared/duration";
import { DurationField } from "./DurationField";

/**
 * Slider positions. Index 0 is the unlimited detent; index 1 upward are real
 * durations, fine-grained where people actually choose.
 */
const STEPS: number[] = (() => {
  const steps = [0];
  for (let m = 1; m <= 60; m++) steps.push(m * 60);
  for (let m = 65; m <= 480; m += 5) steps.push(m * 60);
  return steps;
})();

const UNLIMITED_INDEX = 0;
const MAX_INDEX = STEPS.length - 1;

/** Nearest slider position for a duration in seconds. */
function toIndex(seconds: number | null): number {
  if (seconds === null || seconds === 0) return UNLIMITED_INDEX;
  let best = 1;
  let bestDelta = Infinity;
  for (let i = 1; i < STEPS.length; i++) {
    const delta = Math.abs(STEPS[i] - seconds);
    if (delta < bestDelta) {
      bestDelta = delta;
      best = i;
    }
  }
  return best;
}

export interface InheritedValue {
  label: string;
  seconds: number;
}

interface Props {
  label: string;
  /** Seconds; null when this subject does not set the value itself. */
  value: number | null;
  /** Called continuously during a drag, then once more on release. */
  onChange: (seconds: number | null) => void;
  /** Called on release, so the caller can end the coalescing gesture. */
  onCommit?: () => void;
  /** What applies when this is unset — service default, or the group's value. */
  inherited?: InheritedValue[];
  /** Wording for the zero detent: "Unlimited" for a quota, "None" for a cooldown. */
  unlimitedLabel?: string;
  helperText?: string;
}

export function DurationSlider({
  label,
  value,
  onChange,
  onCommit,
  inherited = [],
  unlimitedLabel = "Unlimited",
  helperText,
}: Props) {
  const index = toIndex(value);

  const marks = inherited
    .filter((i) => i.seconds > 0)
    .map((i) => ({ value: toIndex(i.seconds), label: "" }));

  return (
    <Box>
      <Stack direction="row" spacing={1} sx={{ alignItems: "baseline", justifyContent: "space-between" }}>
        <Typography variant="body2" sx={{ fontWeight: 600 }}>
          {label}
        </Typography>
        <DurationField
          value={value}
          onChange={onChange}
          placeholder={inherited[0] ? formatDurationHuman(inherited[0].seconds) : unlimitedLabel}
        />
      </Stack>

      <Box sx={{ px: 1 }}>
        <Slider
          value={index}
          min={UNLIMITED_INDEX}
          max={MAX_INDEX}
          step={1}
          marks={marks}
          size="small"
          valueLabelDisplay="auto"
          valueLabelFormat={(i) =>
            i === UNLIMITED_INDEX ? unlimitedLabel : formatDurationHuman(STEPS[i])
          }
          onChange={(_, next) => {
            const i = next as number;
            onChange(i === UNLIMITED_INDEX ? 0 : STEPS[i]);
          }}
          onChangeCommitted={() => onCommit?.()}
          aria-label={label}
          getAriaValueText={(i) =>
            i === UNLIMITED_INDEX ? unlimitedLabel : formatDurationHuman(STEPS[i])
          }
          sx={{
            "& .MuiSlider-mark": {
              height: 10,
              width: 2,
              backgroundColor: "warning.main",
            },
          }}
        />
      </Box>

      <Stack direction="row" sx={{ justifyContent: "space-between" }}>
        <Typography variant="caption" color="text.secondary">
          {value === null
            ? inherited[0]
              ? `Not set — ${inherited[0].label} applies (${formatDurationHuman(inherited[0].seconds)})`
              : `Not set — ${unlimitedLabel.toLowerCase()}`
            : value === 0
              ? unlimitedLabel
              : formatDurationHuman(value)}
        </Typography>
        {inherited.length > 0 && (
          <Tooltip
            title={inherited
              .map((i) => `${i.label}: ${formatDurationHuman(i.seconds)}`)
              .join(" · ")}
          >
            <Typography variant="caption" color="warning.main">
              inherited marks
            </Typography>
          </Tooltip>
        )}
      </Stack>
      {helperText && (
        <Typography variant="caption" color="text.secondary" sx={{ display: "block" }}>
          {helperText}
        </Typography>
      )}
    </Box>
  );
}
