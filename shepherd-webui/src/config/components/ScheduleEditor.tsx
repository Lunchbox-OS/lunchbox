/**
 * The schedule grid wired to the document.
 *
 * Owns every patch a schedule edit produces, so `ScheduleGrid` stays a pure
 * rendering + gesture component. Grid gestures commit once, on release: a drag
 * is one undo step without needing coalescing.
 */
import { useState } from "react";
import Alert from "@mui/material/Alert";
import Box from "@mui/material/Box";
import Button from "@mui/material/Button";
import Card from "@mui/material/Card";
import CardContent from "@mui/material/CardContent";
import Chip from "@mui/material/Chip";
import Divider from "@mui/material/Divider";
import FormControlLabel from "@mui/material/FormControlLabel";
import IconButton from "@mui/material/IconButton";
import Stack from "@mui/material/Stack";
import Switch from "@mui/material/Switch";
import TextField from "@mui/material/TextField";
import ToggleButton from "@mui/material/ToggleButton";
import ToggleButtonGroup from "@mui/material/ToggleButtonGroup";
import Typography from "@mui/material/Typography";
import AddIcon from "@mui/icons-material/Add";
import DeleteIcon from "@mui/icons-material/DeleteOutlined";
import { useConfigDoc } from "../doc/ConfigDocProvider";
import { insert, set, subjectPath, unset, windowsPath, type Subject } from "../doc/patches";
import type { RawAvailability, RawTimeWindow } from "../model/config.generated";
import {
  ALL_DAYS,
  DAY_LABELS,
  WEEKDAYS,
  WEEKENDS,
  describeDays,
  formatDays,
  formatTime,
  parseDays,
  parseTime,
  parseWindow,
  toggleDay,
} from "../model/windows";
import { ScheduleGrid } from "./ScheduleGrid";
import { FIELD_DEFAULTS } from "../model/field-defaults.generated";

interface Props {
  subject: Subject;
  availability: RawAvailability | null | undefined;
  /** Snap granularity in minutes. */
  step?: number;
}

export function ScheduleEditor({ subject, availability, step = 15 }: Props) {
  const { apply, availabilityFor } = useConfigDoc();
  const [selected, setSelected] = useState<number | null>(null);

  const windows = availability?.windows ?? [];
  const always = availability?.always ?? FIELD_DEFAULTS.RawAvailability.always;
  const view = availabilityFor(subject);
  const path = (...rest: string[]) => subjectPath(subject, ...rest);
  const winPath = (i: number, ...rest: string[]) =>
    [`${windowsPath(subject)}[${i}]`, ...rest].join(".");

  const addWindow = (mask: number, start: number, end: number) => {
    apply(
      insert(windowsPath(subject), {
        days: formatDays(mask) as string | string[],
        start: formatTime(start),
        end: formatTime(end),
      }),
    );
    setSelected(windows.length);
  };

  const resizeWindow = (index: number, start: number, end: number) => {
    apply(set(winPath(index, "start"), formatTime(start)));
    apply(set(winPath(index, "end"), formatTime(end)));
  };

  /**
   * Alt-drag on one day of a multi-day window: that day leaves the original
   * and becomes its own window, so it can move independently.
   */
  const splitDay = (index: number, day: number, start: number, end: number) => {
    const w = parseWindow(windows[index], index);
    const remaining = w.mask & ~(1 << day);
    if (remaining === 0) {
      resizeWindow(index, start, end);
      return;
    }
    apply(set(winPath(index, "days"), formatDays(remaining, windows[index].days) as string | string[]));
    apply(
      insert(windowsPath(subject), {
        days: formatDays(1 << day) as string | string[],
        start: formatTime(start),
        end: formatTime(end),
      }),
    );
  };

  const removeWindow = (index: number) => {
    apply(unset(`${windowsPath(subject)}[${index}]`));
    setSelected(null);
  };

  const setAlways = (next: boolean) => {
    if (next) apply(set(path("availability", "always"), true));
    else apply(unset(path("availability", "always")));
  };

  const selectedWindow = selected !== null ? windows[selected] : undefined;

  return (
    <Stack spacing={2}>
      <Stack direction="row" spacing={2} sx={{ alignItems: "center", flexWrap: "wrap" }}>
        <FormControlLabel
          control={<Switch checked={always} onChange={(e) => setAlways(e.target.checked)} />}
          label="Always available"
        />
        <Button
          size="small"
          startIcon={<AddIcon />}
          onClick={() => addWindow(WEEKDAYS, 16 * 60, 18 * 60)}
          disabled={always}
        >
          Add window
        </Button>
        <Typography variant="caption" color="text.secondary">
          Drag on the grid to create · drag edges to resize · Alt-drag to split one day out
        </Typography>
      </Stack>

      {always && windows.length > 0 && (
        <Alert severity="info">
          <strong>Always available</strong> overrides the {windows.length} window
          {windows.length === 1 ? "" : "s"} below — they are kept in the file but have no
          effect until you turn it off.
        </Alert>
      )}

      {view && view.invalid_windows.length > 0 && (
        <Alert severity="error">
          {view.invalid_windows.length} window
          {view.invalid_windows.length === 1 ? " has" : "s have"} a day or time the daemon
          cannot read, so {view.invalid_windows.length === 1 ? "it is" : "they are"} not drawn
          below. Fix {view.invalid_windows.length === 1 ? "it" : "them"} in the list.
        </Alert>
      )}

      <Box sx={{ opacity: always ? 0.45 : 1 }}>
        <ScheduleGrid
          windows={windows}
          availability={view}
          step={step}
          selfLabel={subject.kind === "group" ? "This category" : "This activity"}
          selected={selected}
          onSelect={setSelected}
          onCreate={addWindow}
          onResize={resizeWindow}
          onSplitDay={splitDay}
          readOnly={always}
        />
      </Box>

      {selectedWindow && selected !== null && (
        <Card variant="outlined">
          <CardContent>
            <WindowDetail
              window={selectedWindow}
              onChangeDays={(mask) =>
                apply(
                  set(
                    winPath(selected, "days"),
                    formatDays(mask, selectedWindow.days) as string | string[],
                  ),
                )
              }
              onChangeTime={(field, value) => apply(set(winPath(selected, field), value))}
              onDelete={() => removeWindow(selected)}
            />
          </CardContent>
        </Card>
      )}

      {windows.length > 0 && (
        <Box>
          <Typography variant="subtitle2" sx={{ mb: 1 }}>
            Windows
          </Typography>
          <Stack spacing={0.5}>
            {windows.map((w, i) => {
              const parsed = parseWindow(w, i);
              return (
                <Stack
                  key={i}
                  direction="row"
                  spacing={1}
                  onClick={() => setSelected(i)}
                  sx={{ alignItems: "center",
                    px: 1,
                    py: 0.5,
                    borderRadius: 1,
                    cursor: "pointer",
                    backgroundColor: selected === i ? "action.selected" : "transparent",
                    "&:hover": { backgroundColor: "action.hover" },
                  }}
                >
                  <Typography variant="body2" sx={{ flex: 1 }}>
                    {parsed.valid ? describeDays(parsed.mask) : "Unreadable"} · {w.start}–{w.end}
                  </Typography>
                  {parsed.wraps && <Chip size="small" label="past midnight" color="warning" />}
                  {!parsed.valid && <Chip size="small" label="invalid" color="error" />}
                  <IconButton
                    size="small"
                    aria-label={`Delete window ${i + 1}`}
                    onClick={(e) => {
                      e.stopPropagation();
                      removeWindow(i);
                    }}
                  >
                    <DeleteIcon fontSize="small" />
                  </IconButton>
                </Stack>
              );
            })}
          </Stack>
        </Box>
      )}
    </Stack>
  );
}

/**
 * Exact editing for the selected window: day toggles and `HH:MM` fields.
 *
 * This is the keyboard path for everything the grid does by dragging, and the
 * faster path for exact times even with a mouse.
 */
function WindowDetail({
  window: w,
  onChangeDays,
  onChangeTime,
  onDelete,
}: {
  window: RawTimeWindow;
  onChangeDays: (mask: number) => void;
  onChangeTime: (field: "start" | "end", value: string) => void;
  onDelete: () => void;
}) {
  const mask = parseDays(w.days) ?? 0;
  const startValid = parseTime(w.start) !== null;
  const endValid = parseTime(w.end) !== null;
  const wraps = startValid && endValid && (parseTime(w.start) as number) > (parseTime(w.end) as number);

  return (
    <Stack spacing={2}>
      <Stack direction="row" sx={{ alignItems: "center", justifyContent: "space-between" }}>
        <Typography variant="subtitle2">Selected window</Typography>
        <Button size="small" color="error" startIcon={<DeleteIcon />} onClick={onDelete}>
          Delete
        </Button>
      </Stack>

      <Box>
        <Typography variant="caption" color="text.secondary" sx={{ display: "block", mb: 0.5 }}>
          Days
        </Typography>
        <Stack direction="row" spacing={1} useFlexGap sx={{ flexWrap: "wrap" }}>
          <ToggleButtonGroup size="small" value={DAY_LABELS.filter((_, i) => mask & (1 << i))}>
            {DAY_LABELS.map((label, i) => (
              <ToggleButton
                key={label}
                value={label}
                selected={(mask & (1 << i)) !== 0}
                onClick={() => onChangeDays(toggleDay(mask, i))}
              >
                {label}
              </ToggleButton>
            ))}
          </ToggleButtonGroup>
          <Divider orientation="vertical" flexItem />
          <Button size="small" onClick={() => onChangeDays(WEEKDAYS)}>
            Weekdays
          </Button>
          <Button size="small" onClick={() => onChangeDays(WEEKENDS)}>
            Weekends
          </Button>
          <Button size="small" onClick={() => onChangeDays(ALL_DAYS)}>
            Every day
          </Button>
        </Stack>
      </Box>

      <Stack direction="row" spacing={2}>
        <TextField
          label="Start"
          size="small"
          value={w.start}
          error={!startValid}
          helperText={startValid ? " " : "Expected HH:MM"}
          onChange={(e) => onChangeTime("start", e.target.value)}
        />
        <TextField
          label="End"
          size="small"
          value={w.end}
          error={!endValid}
          helperText={endValid ? "Exclusive — the activity closes at this time" : "Expected HH:MM"}
          onChange={(e) => onChangeTime("end", e.target.value)}
        />
      </Stack>

      {wraps && (
        <Alert severity="warning">
          This window ends before it starts, so it wraps past midnight. The daemon checks
          the day against the moment being evaluated, so this means{" "}
          <strong>
            00:00–{w.end} and {w.start}–24:00 on {describeDays(mask).toLowerCase()}
          </strong>{" "}
          — not an overnight window running into the next day. Add a second window on the
          following day if that is what you meant.
        </Alert>
      )}

      {mask === 0 && (
        <Alert severity="error">
          No days selected, so this window never applies.
        </Alert>
      )}
    </Stack>
  );
}
