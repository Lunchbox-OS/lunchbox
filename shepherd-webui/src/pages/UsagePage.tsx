import { useState } from "react";
import Box from "@mui/material/Box";
import Stack from "@mui/material/Stack";
import ToggleButton from "@mui/material/ToggleButton";
import ToggleButtonGroup from "@mui/material/ToggleButtonGroup";
import Typography from "@mui/material/Typography";
import { styled } from "@mui/material/styles";
import { useQuery } from "@tanstack/react-query";
import { getUsage } from "../api/client";
import { formatDurationHuman, type UsageStat } from "../api/types";
import { Spinner } from "../components/Spinner";

type Range = "today" | "7d" | "30d";

function dateRange(range: Range): { from: string; to: string } {
  const to = new Date();
  const from = new Date();
  if (range === "7d") from.setDate(from.getDate() - 6);
  else if (range === "30d") from.setDate(from.getDate() - 29);
  return { from: from.toISOString().slice(0, 10), to: to.toISOString().slice(0, 10) };
}

const BarFill = styled(Box)(({ theme }) => ({
  height: "100%",
  borderRadius: theme.shape.borderRadius,
  background: theme.palette.primary.main,
  transition: "width 0.3s ease",
  minWidth: 4,
}));

const DailyFill = styled(Box)(({ theme }) => ({
  width: "100%",
  borderRadius: `${theme.shape.borderRadius}px ${theme.shape.borderRadius}px 0 0`,
  background: theme.palette.primary.main,
  transition: "height 0.3s ease",
  minHeight: 4,
}));

export function UsagePage() {
  const [range, setRange] = useState<Range>("7d");
  const { from, to } = dateRange(range);

  const { data, isPending: loading } = useQuery({
    queryKey: ["usage", from, to],
    queryFn: () => getUsage(from, to),
  });

  const byEntry = new Map<string, { label: string; secs: number }>();
  for (const stat of data ?? []) {
    const prev = byEntry.get(stat.entry_id) ?? { label: stat.label, secs: 0 };
    byEntry.set(stat.entry_id, { label: stat.label, secs: prev.secs + stat.duration_seconds });
  }
  const sorted = [...byEntry.values()].sort((a, b) => b.secs - a.secs);
  const maxSecs = sorted[0]?.secs ?? 1;

  const byDate = new Map<string, number>();
  for (const stat of data ?? []) {
    byDate.set(stat.date, (byDate.get(stat.date) ?? 0) + stat.duration_seconds);
  }
  const dailyEntries = [...byDate.entries()]
    .sort((a, b) => a[0].localeCompare(b[0]))
    .map(([date, secs]) => ({ date, secs }));
  const maxDailySecs = Math.max(...dailyEntries.map((d) => d.secs), 1);

  const totalSecs = sorted.reduce((acc, e) => acc + e.secs, 0);

  return (
    <Box sx={{ display: "flex", flexDirection: "column", gap: 2 }}>
      <Typography variant="h6" sx={{ fontWeight: 700 }}>Screen Time</Typography>

      <ToggleButtonGroup
        value={range}
        exclusive
        onChange={(_, v) => v && setRange(v)}
        size="small"
        fullWidth
      >
        <ToggleButton value="today">Today</ToggleButton>
        <ToggleButton value="7d">7 Days</ToggleButton>
        <ToggleButton value="30d">30 Days</ToggleButton>
      </ToggleButtonGroup>

      {loading && !data && (
        <Box sx={{ display: "flex", justifyContent: "center", py: 6 }}>
          <Spinner />
        </Box>
      )}

      {!loading && sorted.length === 0 && (
        <Typography color="text.secondary" sx={{ textAlign: "center", py: 4 }}>
          No usage recorded for this period
        </Typography>
      )}

      {sorted.length > 0 && (
        <>
          <Box sx={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
            <Typography variant="body2" color="text.secondary">Total</Typography>
            <Typography variant="body1" sx={{ fontWeight: 700 }}>{formatDurationHuman(totalSecs)}</Typography>
          </Box>

          <Box>
            <Typography variant="subtitle2" color="text.secondary" sx={{ mb: 1 }}>
              By Activity
            </Typography>
            <Stack spacing={1}>
              {sorted.map((entry) => (
                <Box key={entry.label}>
                  <Box sx={{ display: "flex", justifyContent: "space-between", mb: 0.25 }}>
                    <Typography variant="body2">{entry.label}</Typography>
                    <Typography variant="body2" color="text.secondary">
                      {formatDurationHuman(entry.secs)}
                    </Typography>
                  </Box>
                  <Box sx={{ height: 8, borderRadius: 1, bgcolor: "action.hover", overflow: "hidden" }}>
                    <BarFill sx={{ width: `${(entry.secs / maxSecs) * 100}%` }} />
                  </Box>
                </Box>
              ))}
            </Stack>
          </Box>

          {dailyEntries.length > 1 && (
            <Box>
              <Typography variant="subtitle2" color="text.secondary" sx={{ mb: 1 }}>
                Daily Total
              </Typography>
              <Box sx={{ display: "flex", alignItems: "flex-end", gap: 0.5, height: 120 }}>
                {dailyEntries.map(({ date, secs }) => (
                  <Box
                    key={date}
                    sx={{ flex: 1, display: "flex", flexDirection: "column", alignItems: "center", height: "100%" }}
                  >
                    <Box sx={{ flex: 1, width: "100%", display: "flex", alignItems: "flex-end" }}>
                      <DailyFill sx={{ height: `${(secs / maxDailySecs) * 100}%` }} />
                    </Box>
                    <Typography variant="caption" color="text.secondary" sx={{ mt: 0.5, fontSize: "0.6rem" }}>
                      {new Date(date + "T12:00:00").toLocaleDateString([], { weekday: "short" })}
                    </Typography>
                  </Box>
                ))}
              </Box>
            </Box>
          )}
        </>
      )}
    </Box>
  );
}
