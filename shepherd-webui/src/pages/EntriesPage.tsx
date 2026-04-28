import { useState } from "react";
import Alert from "@mui/material/Alert";
import Box from "@mui/material/Box";
import Button from "@mui/material/Button";
import Card from "@mui/material/Card";
import CardContent from "@mui/material/CardContent";
import Chip from "@mui/material/Chip";
import InputAdornment from "@mui/material/InputAdornment";
import Snackbar from "@mui/material/Snackbar";
import Stack from "@mui/material/Stack";
import TextField from "@mui/material/TextField";
import Typography from "@mui/material/Typography";
import SearchIcon from "@mui/icons-material/Search";
import {
  deleteOverride,
  launchSession,
  listEntries,
  listOverrides,
  upsertOverride,
} from "../api/client";
import {
  durationToSecs,
  formatDurationHuman,
  reasonLabel,
  type DailyOverride,
  type EntryView,
} from "../api/types";
import { useApi } from "../hooks/useApi";
import { useEvents } from "../hooks/useEvents";
import { Spinner } from "../components/Spinner";

function todayString(): string {
  return new Date().toISOString().slice(0, 10);
}

export function EntriesPage() {
  const { data: entries, loading: entriesLoading, refetch: refetchEntries } = useApi(listEntries, []);
  const { data: overrides, refetch: refetchOverrides } = useApi(
    () => listOverrides(todayString()),
    [],
  );

  const [busyId, setBusyId] = useState<string | null>(null);
  const [msg, setMsg] = useState<{ text: string; ok: boolean } | null>(null);
  const [search, setSearch] = useState("");

  useEvents(() => { refetchEntries(); refetchOverrides(); });

  const flash = (text: string, ok = true) => {
    setMsg({ text, ok });
    setTimeout(() => setMsg(null), 3000);
  };

  const handleLaunch = async (entry_id: string) => {
    setBusyId(entry_id);
    try {
      const res = await launchSession(entry_id);
      if (res.result === "approved") {
        flash("Session started!");
        refetchEntries();
      } else {
        const reason = res.reasons[0] ? reasonLabel(res.reasons[0]) : "Denied";
        flash(`Cannot launch: ${reason}`, false);
      }
    } catch (e) {
      flash(String(e), false);
    } finally {
      setBusyId(null);
    }
  };

  const handleDisableToday = async (entry: EntryView) => {
    setBusyId(entry.entry_id);
    try {
      await upsertOverride(entry.entry_id, false, null, todayString());
      flash(`${entry.label}: disabled for today`);
      refetchEntries();
      refetchOverrides();
    } catch (e) {
      flash(String(e), false);
    } finally {
      setBusyId(null);
    }
  };

  const handleClearOverride = async (entry: EntryView) => {
    setBusyId(entry.entry_id);
    try {
      await deleteOverride(entry.entry_id, todayString());
      flash(`${entry.label}: override cleared`);
      refetchEntries();
      refetchOverrides();
    } catch (e) {
      flash(String(e), false);
    } finally {
      setBusyId(null);
    }
  };

  const handleEnableOverride = async (entry: EntryView) => {
    setBusyId(entry.entry_id);
    try {
      await upsertOverride(entry.entry_id, true, null, todayString());
      flash(`${entry.label}: enabled for today`);
      refetchEntries();
      refetchOverrides();
    } catch (e) {
      flash(String(e), false);
    } finally {
      setBusyId(null);
    }
  };

  const loading = entriesLoading && !entries;
  const filtered = (entries ?? []).filter((e) =>
    e.label.toLowerCase().includes(search.toLowerCase()),
  );
  const overrideMap = new Map((overrides ?? []).map((ov) => [ov.entry_id, ov]));

  return (
    <Box sx={{ display: "flex", flexDirection: "column", gap: 2 }}>
      <Typography variant="h6" sx={{ fontWeight: 700 }}>Activities</Typography>

      <Snackbar open={!!msg} autoHideDuration={3000} onClose={() => setMsg(null)}>
        <Alert severity={msg?.ok ? "success" : "error"} onClose={() => setMsg(null)} sx={{ width: "100%" }}>
          {msg?.text}
        </Alert>
      </Snackbar>

      <TextField
        size="small"
        placeholder="Search…"
        value={search}
        onChange={(e) => setSearch(e.target.value)}
        slotProps={{
          input: {
            startAdornment: (
              <InputAdornment position="start">
                <SearchIcon fontSize="small" />
              </InputAdornment>
            ),
          },
        }}
      />

      {loading && (
        <Box sx={{ display: "flex", justifyContent: "center", py: 6 }}>
          <Spinner />
        </Box>
      )}

      <Stack spacing={1.5}>
        {filtered.map((entry) => (
          <EntryCard
            key={entry.entry_id}
            entry={entry}
            override={overrideMap.get(entry.entry_id)}
            busy={busyId === entry.entry_id}
            onLaunch={() => handleLaunch(entry.entry_id)}
            onDisableToday={() => handleDisableToday(entry)}
            onClear={() => handleClearOverride(entry)}
            onEnable={() => handleEnableOverride(entry)}
          />
        ))}
        {!loading && filtered.length === 0 && (
          <Typography color="text.secondary" sx={{ textAlign: "center", py: 4 }}>
            No activities found
          </Typography>
        )}
      </Stack>
    </Box>
  );
}

function EntryCard({
  entry,
  override,
  busy,
  onLaunch,
  onDisableToday,
  onClear,
  onEnable,
}: {
  entry: EntryView;
  override: DailyOverride | undefined;
  busy: boolean;
  onLaunch: () => void;
  onDisableToday: () => void;
  onClear: () => void;
  onEnable: () => void;
}) {
  const manuallyDisabled = override?.availability === false;
  const manuallyEnabled = override?.availability === true;
  const maxRunSecs = durationToSecs(entry.max_run_if_started_now);

  return (
    <Card
      variant="outlined"
      sx={{ opacity: !entry.enabled && !manuallyEnabled ? 0.75 : 1 }}
    >
      <CardContent sx={{ pb: "12px !important" }}>
        <Box sx={{ display: "flex", justifyContent: "space-between", alignItems: "flex-start", mb: 0.5 }}>
          <Box>
            <Typography sx={{ fontWeight: 600 }}>{entry.label}</Typography>
            <Typography variant="caption" color="text.secondary">
              {entry.kind_tag}
            </Typography>
          </Box>
          {manuallyDisabled ? (
            <Chip label="Off today" size="small" color="default" />
          ) : manuallyEnabled ? (
            <Chip label="On today ★" size="small" color="primary" />
          ) : entry.enabled ? (
            <Chip label="Available" size="small" color="success" />
          ) : (
            <Chip label="Unavailable" size="small" color="default" />
          )}
        </Box>

        {!entry.enabled && entry.reasons.length > 0 && (
          <Typography variant="caption" color="text.secondary">
            {reasonLabel(entry.reasons[0])}
          </Typography>
        )}

        {entry.enabled && maxRunSecs > 0 && (
          <Typography variant="caption" color="text.secondary">
            Up to {formatDurationHuman(maxRunSecs)}
          </Typography>
        )}

        {override?.quota_delta_seconds != null && override.quota_delta_seconds !== 0 && (
          <Typography variant="caption" color="text.secondary" sx={{ display: "block" }}>
            Quota {override.quota_delta_seconds > 0 ? "+" : ""}
            {formatDurationHuman(Math.abs(override.quota_delta_seconds))}
          </Typography>
        )}

        <Box sx={{ display: "flex", gap: 1, mt: 1.5, flexWrap: "wrap" }}>
          {(manuallyDisabled || manuallyEnabled) ? (
            <Button size="small" variant="outlined" onClick={onClear} disabled={busy}>
              {busy ? <Spinner size={14} /> : "Clear Override"}
            </Button>
          ) : (
            <>
              <Button size="small" variant="text" color="inherit" onClick={onDisableToday} disabled={busy}>
                {busy ? <Spinner size={14} /> : "Disable Today"}
              </Button>
              {!entry.enabled && (
                <Button size="small" variant="outlined" color="primary" onClick={onEnable} disabled={busy}>
                  {busy ? <Spinner size={14} /> : "Enable Today"}
                </Button>
              )}
            </>
          )}
          {entry.enabled && (
            <Button size="small" variant="contained" onClick={onLaunch} disabled={busy}>
              {busy ? <Spinner size={14} /> : "Launch"}
            </Button>
          )}
        </Box>
      </CardContent>
    </Card>
  );
}
