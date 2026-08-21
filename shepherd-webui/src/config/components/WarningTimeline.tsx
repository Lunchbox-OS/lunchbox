/**
 * Warning thresholds as markers on the session they belong to.
 *
 * The bar is as long as the session's own `max_run_seconds`, so a threshold
 * cannot be dragged past the end. That is `WarningExceedsMaxRun` made
 * unrepresentable rather than merely diagnosed — the validator still catches it
 * for files edited by hand, but nothing here can produce it.
 */
import { useMemo, useRef, useState } from "react";
import Alert from "@mui/material/Alert";
import Box from "@mui/material/Box";
import Button from "@mui/material/Button";
import IconButton from "@mui/material/IconButton";
import MenuItem from "@mui/material/MenuItem";
import Stack from "@mui/material/Stack";
import TextField from "@mui/material/TextField";
import Tooltip from "@mui/material/Tooltip";
import Typography from "@mui/material/Typography";
import AddIcon from "@mui/icons-material/Add";
import DeleteIcon from "@mui/icons-material/DeleteOutlined";
import { alpha, useTheme } from "@mui/material/styles";
import { formatDurationHuman } from "../../shared/duration";
import type { RawWarningThreshold } from "../model/config.generated";
import { DurationField } from "./DurationField";

const SEVERITIES = ["info", "warn", "critical"] as const;
type Severity = (typeof SEVERITIES)[number];

interface Props {
  warnings: RawWarningThreshold[];
  /** Session length the markers sit on. Null when unlimited. */
  maxRunSeconds: number | null;
  onChange: (index: number, next: Partial<RawWarningThreshold>) => void;
  onAdd: (warning: RawWarningThreshold) => void;
  onRemove: (index: number) => void;
  /** Called when a drag finishes, to close the coalescing gesture. */
  onCommit?: () => void;
}

export function WarningTimeline({
  warnings,
  maxRunSeconds,
  onChange,
  onAdd,
  onRemove,
  onCommit,
}: Props) {
  const theme = useTheme();
  const barRef = useRef<HTMLDivElement>(null);
  const [dragging, setDragging] = useState<number | null>(null);

  // With no session cap there is nothing to measure against, so fall back to a
  // notional hour purely for layout.
  const span = maxRunSeconds && maxRunSeconds > 0 ? maxRunSeconds : 3600;

  const colorFor = (s: string) =>
    s === "critical"
      ? theme.palette.error.main
      : s === "info"
        ? theme.palette.info.main
        : theme.palette.warning.main;

  const secondsAt = (clientX: number): number => {
    const rect = barRef.current?.getBoundingClientRect();
    if (!rect) return 0;
    const ratio = 1 - (clientX - rect.left) / rect.width;
    // Clamped to the session, which is what makes the invalid state
    // unreachable by dragging.
    return Math.max(0, Math.min(span, Math.round((ratio * span) / 10) * 10));
  };

  const overLong = useMemo(
    () =>
      maxRunSeconds && maxRunSeconds > 0
        ? warnings.filter((w) => w.seconds_before >= maxRunSeconds).length
        : 0,
    [warnings, maxRunSeconds],
  );

  return (
    <Stack spacing={1.5}>
      <Stack direction="row" sx={{ alignItems: "center", justifyContent: "space-between" }}>
        <Typography variant="body2" sx={{ fontWeight: 600 }}>
          Warnings before time is up
        </Typography>
        <Button
          size="small"
          startIcon={<AddIcon />}
          onClick={() => onAdd({ seconds_before: 300, severity: "warn" })}
        >
          Add warning
        </Button>
      </Stack>

      <Box
        ref={barRef}
        onPointerMove={(e) => {
          if (dragging === null) return;
          onChange(dragging, { seconds_before: secondsAt(e.clientX) });
        }}
        onPointerUp={() => {
          if (dragging !== null) onCommit?.();
          setDragging(null);
        }}
        onPointerLeave={() => setDragging(null)}
        sx={{
          position: "relative",
          height: 46,
          borderRadius: 1,
          border: "1px solid",
          borderColor: "divider",
          background: `linear-gradient(to right, ${alpha(
            theme.palette.success.main,
            0.18,
          )}, ${alpha(theme.palette.error.main, 0.18)})`,
        }}
      >
        {warnings.map((w, i) => {
          const ratio = 1 - Math.min(1, w.seconds_before / span);
          const severity = (w.severity ?? "warn") as Severity;
          return (
            <Tooltip
              key={i}
              title={`${formatDurationHuman(w.seconds_before)} before the end · ${severity}`}
            >
              <Box
                onPointerDown={(e) => {
                  (e.target as Element).setPointerCapture?.(e.pointerId);
                  setDragging(i);
                }}
                role="slider"
                aria-label={`Warning ${i + 1}`}
                aria-valuenow={w.seconds_before}
                aria-valuemin={0}
                aria-valuemax={span}
                tabIndex={0}
                onKeyDown={(e) => {
                  const stepSize = e.shiftKey ? 300 : 30;
                  if (e.key === "ArrowLeft")
                    onChange(i, {
                      seconds_before: Math.min(span, w.seconds_before + stepSize),
                    });
                  if (e.key === "ArrowRight")
                    onChange(i, {
                      seconds_before: Math.max(0, w.seconds_before - stepSize),
                    });
                }}
                sx={{
                  position: "absolute",
                  left: `${ratio * 100}%`,
                  top: 4,
                  bottom: 4,
                  width: 10,
                  ml: "-5px",
                  borderRadius: 1,
                  backgroundColor: colorFor(severity),
                  cursor: "ew-resize",
                  outlineOffset: 2,
                }}
              />
            </Tooltip>
          );
        })}
        <Typography
          variant="caption"
          sx={{ position: "absolute", left: 6, bottom: 2, color: "text.secondary" }}
        >
          session start
        </Typography>
        <Typography
          variant="caption"
          sx={{ position: "absolute", right: 6, bottom: 2, color: "text.secondary" }}
        >
          {maxRunSeconds ? formatDurationHuman(maxRunSeconds) : "no cap"}
        </Typography>
      </Box>

      {!maxRunSeconds && warnings.length > 0 && (
        <Alert severity="info">
          There is no session cap here, so the bar above is only a sketch. Warnings still
          fire against whatever cap the category or service default supplies.
        </Alert>
      )}

      {overLong > 0 && (
        <Alert severity="warning">
          {overLong} warning{overLong === 1 ? "" : "s"} fire at or before the session even
          starts, so {overLong === 1 ? "it is" : "they are"} skipped. Drag{" "}
          {overLong === 1 ? "it" : "them"} to the right.
        </Alert>
      )}

      <Stack spacing={1}>
        {warnings.map((w, i) => (
          <Stack key={i} direction="row" spacing={1} sx={{ alignItems: "center" }}>
            <DurationField
              label="Before the end"
              value={w.seconds_before}
              clearable={false}
              onChange={(secs) => secs !== null && onChange(i, { seconds_before: secs })}
            />
            <TextField
              select
              size="small"
              label="Severity"
              value={w.severity ?? "warn"}
              onChange={(e) => onChange(i, { severity: e.target.value })}
              sx={{ minWidth: 120 }}
            >
              {SEVERITIES.map((s) => (
                <MenuItem key={s} value={s}>
                  {s}
                </MenuItem>
              ))}
            </TextField>
            <TextField
              size="small"
              label="Message (optional)"
              value={w.message ?? ""}
              placeholder="Closing in {remaining} seconds!"
              onChange={(e) => onChange(i, { message: e.target.value || null })}
              sx={{ flex: 1 }}
            />
            <IconButton
              size="small"
              aria-label={`Remove warning ${i + 1}`}
              onClick={() => onRemove(i)}
            >
              <DeleteIcon fontSize="small" />
            </IconButton>
          </Stack>
        ))}
      </Stack>
    </Stack>
  );
}
