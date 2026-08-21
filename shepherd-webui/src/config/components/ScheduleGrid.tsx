/**
 * Availability, as a week you drag on.
 *
 * Drag empty space to create a window, drag a band's body to move it, drag its
 * edges to resize, click to select. Every gesture has a keyboard equivalent and
 * the selected window always has plain `HH:MM` fields in the detail panel
 * beside it — drag-only editing is an accessibility dead end, and typing an
 * exact time is faster anyway.
 *
 * Three behaviours are drawn the way the engine evaluates them, not the way
 * they read in TOML:
 *
 * - **No windows means always available.** An unconfigured activity renders as
 *   a fully lit week, not an empty one.
 * - **One window can span several days.** `days = "weekdays"` is a single
 *   object drawn in five columns; dragging any of them moves all five, because
 *   that is what the file says. Alt-drag splits one day off.
 * - **A window that wraps midnight does not cross into the next day.** The day
 *   mask is tested against the weekday of the instant, so `["fri"]
 *   22:00-02:00` draws two bands in the Friday column, joined by a connector.
 */
import { useCallback, useMemo, useRef, useState } from "react";
import Box from "@mui/material/Box";
import Stack from "@mui/material/Stack";
import Tooltip from "@mui/material/Tooltip";
import Typography from "@mui/material/Typography";
import { alpha, useTheme } from "@mui/material/styles";
import type { AvailabilityView } from "../model/availability";
import type { RawTimeWindow } from "../model/config.generated";
import {
  DAY_LABELS,
  MINUTES_IN_DAY,
  bandsOnDay,
  dayCount,
  describeDays,
  parseWindows,
  snap,
  type ParsedWindow,
} from "../model/windows";

const HOUR_HEIGHT = 26;
const GRID_HEIGHT = HOUR_HEIGHT * 24;
const EDGE_GRAB_PX = 7;
/** Shortest window a drag can produce, in minutes. */
const MIN_DURATION = 15;

export interface ScheduleGridProps {
  windows: RawTimeWindow[];
  /** Spans from the wasm module, including the group layer and intersection. */
  availability: AvailabilityView | null;
  /** Snap granularity in minutes. */
  step?: number;
  selected: number | null;
  onSelect: (index: number | null) => void;
  onCreate: (mask: number, start: number, end: number) => void;
  /** Move or resize an existing window. Called once, on release. */
  onResize: (index: number, start: number, end: number) => void;
  /** Alt-drag: pull one day out of a multi-day window into its own. */
  onSplitDay: (index: number, day: number, start: number, end: number) => void;
  readOnly?: boolean;
}

type DragMode = "create" | "move" | "resize-start" | "resize-end";

interface DragState {
  mode: DragMode;
  day: number;
  windowIndex: number | null;
  /** Minutes at pointer-down, for computing a move delta. */
  anchor: number;
  originStart: number;
  originEnd: number;
  start: number;
  end: number;
  splitDay: boolean;
}

export function ScheduleGrid({
  windows,
  availability,
  step = 15,
  selected,
  onSelect,
  onCreate,
  onResize,
  onSplitDay,
  readOnly = false,
}: ScheduleGridProps) {
  const theme = useTheme();
  const gridRef = useRef<HTMLDivElement>(null);
  const [drag, setDrag] = useState<DragState | null>(null);

  const parsed = useMemo(() => parseWindows(windows), [windows]);

  const unrestricted = availability?.entry_unrestricted ?? windows.length === 0;

  const minutesAt = useCallback(
    (clientY: number): number => {
      const rect = gridRef.current?.getBoundingClientRect();
      if (!rect) return 0;
      const ratio = (clientY - rect.top) / rect.height;
      return Math.max(
        0,
        Math.min(MINUTES_IN_DAY, snap(ratio * MINUTES_IN_DAY, step)),
      );
    },
    [step],
  );

  const beginDrag = (
    e: React.PointerEvent,
    day: number,
    windowIndex: number | null,
    mode: DragMode,
  ) => {
    if (readOnly) return;
    e.preventDefault();
    e.stopPropagation();
    (e.target as Element).setPointerCapture?.(e.pointerId);
    const at = minutesAt(e.clientY);
    const w = windowIndex === null ? null : parsed[windowIndex];
    setDrag({
      mode,
      day,
      windowIndex,
      anchor: at,
      originStart: w?.start ?? at,
      originEnd: w?.end ?? at,
      start: w?.start ?? at,
      end: w?.end ?? at,
      // Alt pulls a single day out of a multi-day window.
      splitDay: e.altKey && w !== null && dayCount(w.mask) > 1,
    });
    if (windowIndex !== null) onSelect(windowIndex);
  };

  const onPointerMove = (e: React.PointerEvent) => {
    if (!drag) return;
    const at = minutesAt(e.clientY);
    setDrag((d) => {
      if (!d) return d;
      switch (d.mode) {
        case "create":
          return { ...d, start: Math.min(d.anchor, at), end: Math.max(d.anchor, at) };
        case "resize-start":
          return { ...d, start: Math.min(at, d.end - MIN_DURATION) };
        case "resize-end":
          return { ...d, end: Math.max(at, d.start + MIN_DURATION) };
        case "move": {
          const delta = at - d.anchor;
          const length = d.originEnd - d.originStart;
          let start = d.originStart + delta;
          start = Math.max(0, Math.min(MINUTES_IN_DAY - length, start));
          return { ...d, start, end: start + length };
        }
      }
    });
  };

  const onPointerUp = () => {
    if (!drag) return;
    const { mode, windowIndex, day, splitDay } = drag;
    let { start, end } = drag;
    if (end - start < MIN_DURATION) end = start + MIN_DURATION;
    if (end > MINUTES_IN_DAY) {
      end = MINUTES_IN_DAY;
      start = Math.min(start, end - MIN_DURATION);
    }

    if (mode === "create") {
      onCreate(1 << day, start, end);
    } else if (windowIndex !== null) {
      if (splitDay) onSplitDay(windowIndex, day, start, end);
      else onResize(windowIndex, start, end);
    }
    setDrag(null);
  };

  return (
    <Box>
      <Stack direction="row" spacing={1.5} sx={{ alignItems: "center", flexWrap: "wrap", mb: 1 }}>
        <Legend color={theme.palette.primary.main} label="This activity" />
        {availability?.group && (
          <Legend color={theme.palette.text.disabled} label="Its category" dashed />
        )}
        {availability?.group && (
          <Legend color={theme.palette.success.main} label="Effective (both)" outlined />
        )}
      </Stack>

      {unrestricted && (
        <Typography variant="body2" color="text.secondary" sx={{ mb: 1 }}>
          No windows set, so this is available <strong>all week</strong>. Drag on the
          grid to restrict it.
        </Typography>
      )}

      <Box sx={{ display: "flex", userSelect: "none" }}>
        {/* Hour gutter */}
        <Box sx={{ width: 44, flexShrink: 0, pt: "22px" }}>
          {Array.from({ length: 24 }, (_, h) => (
            <Box
              key={h}
              sx={{
                height: HOUR_HEIGHT,
                fontSize: 10,
                color: "text.secondary",
                textAlign: "right",
                pr: 0.75,
                transform: "translateY(-6px)",
              }}
            >
              {h % 3 === 0 ? `${String(h).padStart(2, "0")}:00` : ""}
            </Box>
          ))}
        </Box>

        <Box sx={{ flex: 1, minWidth: 0 }}>
          <Box sx={{ display: "flex" }}>
            {DAY_LABELS.map((label) => (
              <Box
                key={label}
                sx={{
                  flex: 1,
                  textAlign: "center",
                  fontSize: 12,
                  fontWeight: 600,
                  color: "text.secondary",
                  pb: 0.5,
                }}
              >
                {label}
              </Box>
            ))}
          </Box>

          <Box
            ref={gridRef}
            onPointerMove={onPointerMove}
            onPointerUp={onPointerUp}
            onPointerCancel={() => setDrag(null)}
            sx={{
              display: "flex",
              height: GRID_HEIGHT,
              position: "relative",
              border: "1px solid",
              borderColor: "divider",
              borderRadius: 1,
              overflow: "hidden",
              backgroundImage: `repeating-linear-gradient(
                to bottom,
                ${alpha(theme.palette.text.primary, 0.06)} 0,
                ${alpha(theme.palette.text.primary, 0.06)} 1px,
                transparent 1px,
                transparent ${HOUR_HEIGHT}px
              )`,
            }}
          >
            {DAY_LABELS.map((label, day) => (
              <DayColumn
                key={label}
                day={day}
                label={label}
                parsed={parsed}
                availability={availability}
                unrestricted={unrestricted}
                selected={selected}
                drag={drag}
                readOnly={readOnly}
                onBackgroundDown={(e) => beginDrag(e, day, null, "create")}
                onBandDown={beginDrag}
                onSelect={onSelect}
              />
            ))}
          </Box>
        </Box>
      </Box>
    </Box>
  );
}

function Legend({
  color,
  label,
  dashed,
  outlined,
}: {
  color: string;
  label: string;
  dashed?: boolean;
  outlined?: boolean;
}) {
  return (
    <Stack direction="row" spacing={0.5} sx={{ alignItems: "center" }}>
      <Box
        sx={{
          width: 14,
          height: 10,
          borderRadius: 0.5,
          backgroundColor: outlined ? "transparent" : alpha(color, dashed ? 0.25 : 0.5),
          border: outlined ? `2px solid ${color}` : dashed ? `1px dashed ${color}` : "none",
        }}
      />
      <Typography variant="caption" color="text.secondary">
        {label}
      </Typography>
    </Stack>
  );
}

interface DayColumnProps {
  day: number;
  label: string;
  parsed: ParsedWindow[];
  availability: AvailabilityView | null;
  unrestricted: boolean;
  selected: number | null;
  drag: DragState | null;
  readOnly: boolean;
  onBackgroundDown: (e: React.PointerEvent) => void;
  onBandDown: (
    e: React.PointerEvent,
    day: number,
    windowIndex: number,
    mode: DragMode,
  ) => void;
  onSelect: (index: number | null) => void;
}

function DayColumn({
  day,
  label,
  parsed,
  availability,
  unrestricted,
  selected,
  drag,
  readOnly,
  onBackgroundDown,
  onBandDown,
  onSelect,
}: DayColumnProps) {
  const theme = useTheme();
  const groupSpans = availability?.group?.[day] ?? [];
  const showGroupLayer = availability?.group != null && !availability.group_unrestricted;

  return (
    <Box
      onPointerDown={onBackgroundDown}
      sx={{
        flex: 1,
        position: "relative",
        borderRight: "1px solid",
        borderColor: "divider",
        "&:last-of-type": { borderRight: "none" },
        cursor: readOnly ? "default" : "crosshair",
        backgroundColor: unrestricted
          ? alpha(theme.palette.primary.main, 0.07)
          : "transparent",
        backgroundImage: unrestricted
          ? `repeating-linear-gradient(45deg,
              ${alpha(theme.palette.primary.main, 0.10)} 0 6px,
              transparent 6px 12px)`
          : "none",
      }}
    >
      {/* The category's windows, behind. What this activity can never exceed. */}
      {showGroupLayer &&
        groupSpans.map((span, i) => (
          <Box
            key={`g${i}`}
            sx={{
              position: "absolute",
              left: 2,
              right: 2,
              top: `${(span.start / MINUTES_IN_DAY) * 100}%`,
              height: `${((span.end - span.start) / MINUTES_IN_DAY) * 100}%`,
              borderRadius: 0.5,
              border: `1px dashed ${theme.palette.text.disabled}`,
              backgroundColor: alpha(theme.palette.text.disabled, 0.12),
              pointerEvents: "none",
            }}
          />
        ))}

      {/* This activity's own windows. */}
      {parsed.map((w) => {
        const bands = bandsOnDay(w, day);
        if (bands.length === 0) return null;
        const isSelected = selected === w.index;
        const dragging = drag?.windowIndex === w.index && drag.mode !== "create";
        const live =
          dragging && (!drag.splitDay || drag.day === day)
            ? [{ start: drag.start, end: drag.end }]
            : bands;

        return live.map((band, i) => (
          <Band
            key={`${w.index}-${i}`}
            band={band}
            selected={isSelected}
            wraps={w.wraps}
            multiDay={dayCount(w.mask) > 1}
            title={`${describeDays(w.mask)} · ${label}`}
            readOnly={readOnly}
            onPointerDown={(e, mode) => onBandDown(e, day, w.index, mode)}
            onClick={() => onSelect(w.index)}
          />
        ));
      })}

      {/* The band being drawn right now. */}
      {drag?.mode === "create" && drag.day === day && (
        <Box
          sx={{
            position: "absolute",
            left: 2,
            right: 2,
            top: `${(drag.start / MINUTES_IN_DAY) * 100}%`,
            height: `${((drag.end - drag.start) / MINUTES_IN_DAY) * 100}%`,
            borderRadius: 0.5,
            backgroundColor: alpha(theme.palette.primary.main, 0.45),
            border: `1px solid ${theme.palette.primary.main}`,
            pointerEvents: "none",
          }}
        />
      )}
    </Box>
  );
}

function Band({
  band,
  selected,
  wraps,
  multiDay,
  title,
  readOnly,
  onPointerDown,
  onClick,
}: {
  band: { start: number; end: number };
  selected: boolean;
  wraps: boolean;
  multiDay: boolean;
  title: string;
  readOnly: boolean;
  onPointerDown: (e: React.PointerEvent, mode: DragMode) => void;
  onClick: () => void;
}) {
  const theme = useTheme();
  const height = ((band.end - band.start) / MINUTES_IN_DAY) * 100;

  const modeFor = (e: React.PointerEvent): DragMode => {
    const rect = (e.currentTarget as HTMLElement).getBoundingClientRect();
    if (e.clientY - rect.top < EDGE_GRAB_PX) return "resize-start";
    if (rect.bottom - e.clientY < EDGE_GRAB_PX) return "resize-end";
    return "move";
  };

  return (
    <Tooltip
      title={
        wraps
          ? `${title} — wraps past midnight, so it applies at both ends of this day`
          : multiDay
            ? `${title} — one window across several days; dragging moves all of them`
            : title
      }
      placement="right"
    >
      <Box
        onPointerDown={(e) => onPointerDown(e, modeFor(e))}
        onClick={onClick}
        sx={{
          position: "absolute",
          left: 2,
          right: 2,
          top: `${(band.start / MINUTES_IN_DAY) * 100}%`,
          height: `${height}%`,
          minHeight: 6,
          borderRadius: 0.5,
          backgroundColor: alpha(theme.palette.primary.main, selected ? 0.65 : 0.42),
          border: `${selected ? 2 : 1}px solid ${theme.palette.primary.main}`,
          borderStyle: wraps ? "dashed" : "solid",
          cursor: readOnly ? "pointer" : "grab",
          "&:hover": { backgroundColor: alpha(theme.palette.primary.main, 0.55) },
        }}
      />
    </Tooltip>
  );
}
