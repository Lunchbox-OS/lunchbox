import { useEffect, useState } from "react";
import Box from "@mui/material/Box";
import Button from "@mui/material/Button";
import Card from "@mui/material/Card";
import CardContent from "@mui/material/CardContent";
import Chip from "@mui/material/Chip";
import Slider from "@mui/material/Slider";
import Stack from "@mui/material/Stack";
import Switch from "@mui/material/Switch";
import Typography from "@mui/material/Typography";
import HeadphonesIcon from "@mui/icons-material/Headphones";
import SpeakerIcon from "@mui/icons-material/Speaker";
import TvIcon from "@mui/icons-material/Tv";
import BluetoothIcon from "@mui/icons-material/Bluetooth";
import CableIcon from "@mui/icons-material/Cable";
import AudioFileIcon from "@mui/icons-material/AudioFile";
import type { AudioOutputRecord } from "../api/types";

/**
 * Cap offered when a limit is first switched on, before the parent has touched
 * the slider. Deliberately the same number as the companion's `DEFAULT_CAP`
 * (`ui/device/AudioOutputsCard.kt`): this is one control that happens to be
 * rendered twice, and it used to hand out 80 here and 50 on the phone for the
 * same tap on the same device.
 *
 * Switching a limit on clamps immediately, so the value is chosen to be
 * protective rather than to be a no-op — flipping the switch should visibly do
 * something, and switching it back off undoes it.
 */
const DEFAULT_CAP = 50;

/**
 * The icon is chosen from the advisory `kind`, which is frequently `unknown`
 * (a plain USB interface tells us nothing about itself). That is expected —
 * the description and the "In use now" marker are what the parent actually
 * identifies a device by.
 */
function kindIcon(kind: AudioOutputRecord["output"]["kind"]) {
  switch (kind) {
    case "headphones":
      return <HeadphonesIcon fontSize="small" />;
    case "speakers":
      return <SpeakerIcon fontSize="small" />;
    case "hdmi":
      return <TvIcon fontSize="small" />;
    case "bluetooth":
      return <BluetoothIcon fontSize="small" />;
    case "line_out":
      return <CableIcon fontSize="small" />;
    default:
      return <AudioFileIcon fontSize="small" />;
  }
}

function lastSeenLabel(iso: string): string {
  const then = new Date(iso).getTime();
  if (Number.isNaN(then)) return "";
  const mins = Math.floor((Date.now() - then) / 60000);
  if (mins < 2) return "just now";
  if (mins < 60) return `${mins} min ago`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h ago`;
  return `${Math.floor(hours / 24)}d ago`;
}

function OutputRow({
  record,
  busy,
  onSetLimit,
  onForget,
}: {
  record: AudioOutputRecord;
  busy: boolean;
  onSetLimit: (key: string, max: number | null) => void;
  onForget: (key: string) => void;
}) {
  const capped = record.max_volume !== null;
  // Local slider state so dragging stays smooth; the commit happens on release.
  // Synced only while a cap exists, so switching a limit off and back on
  // restores the number the parent last chose rather than the default.
  const [draft, setDraft] = useState(record.max_volume ?? DEFAULT_CAP);
  useEffect(() => {
    if (record.max_volume !== null) setDraft(record.max_volume);
  }, [record.max_volume]);

  return (
    <Box sx={{ py: 1.5, borderTop: 1, borderColor: "divider" }}>
      <Box sx={{ display: "flex", alignItems: "center", gap: 1, mb: 0.5 }}>
        {kindIcon(record.output.kind)}
        <Typography variant="body2" sx={{ fontWeight: 600, flex: 1 }}>
          {record.output.description || record.output.key}
        </Typography>
        {record.active && <Chip label="In use now" size="small" color="primary" />}
      </Box>

      <Typography variant="caption" color="text.disabled" sx={{ display: "block", mb: 1 }}>
        Last used {lastSeenLabel(record.last_seen)}
      </Typography>

      <Box sx={{ display: "flex", alignItems: "center", gap: 2 }}>
        <Switch
          size="small"
          checked={capped}
          disabled={busy}
          onChange={(e) => onSetLimit(record.output.key, e.target.checked ? draft : null)}
          // `slotProps.input`, not the older `inputProps`: MUI dropped the latter
          // in v7 and silently ignores it, so the switch shipped with no
          // accessible name at all.
          slotProps={{
            input: { "aria-label": `Limit volume for ${record.output.description}` },
          }}
        />
        <Slider
          min={0}
          max={100}
          value={capped ? draft : 100}
          disabled={busy || !capped}
          onChange={(_, v) => setDraft(v as number)}
          onChangeCommitted={(_, v) => onSetLimit(record.output.key, v as number)}
          aria-label={`Maximum volume for ${record.output.description}`}
          sx={{ flex: 1 }}
        />
        <Typography
          variant="body2"
          color={capped ? "text.primary" : "text.disabled"}
          sx={{ minWidth: 64, textAlign: "right" }}
        >
          {capped ? `Max ${draft}%` : "No limit"}
        </Typography>
        <Button
          size="small"
          color="inherit"
          disabled={busy || record.active}
          onClick={() => onForget(record.output.key)}
          // Forgetting the device in use would immediately rediscover it.
          title={record.active ? "Can't forget the device in use" : "Forget this device"}
        >
          Forget
        </Button>
      </Box>
    </Box>
  );
}

export function AudioOutputsCard({
  records,
  busy,
  onSetLimit,
  onForget,
}: {
  records: AudioOutputRecord[] | undefined;
  busy: boolean;
  onSetLimit: (key: string, max: number | null) => void;
  onForget: (key: string) => void;
}) {
  return (
    <Card variant="outlined">
      <CardContent>
        <Typography variant="subtitle1" gutterBottom sx={{ fontWeight: 600 }}>
          Volume limits per device
        </Typography>
        <Typography variant="body2" color="text.secondary">
          Set a different maximum for headphones than for speakers. Devices appear
          here once they have been used, so plug one in if you don't see it. The
          strictest limit always applies — a per-device limit can lower the
          overall limit but never raise it.
        </Typography>

        {!records || records.length === 0 ? (
          <Typography variant="body2" color="text.disabled" sx={{ mt: 2 }}>
            No audio devices seen yet.
          </Typography>
        ) : (
          <Stack sx={{ mt: 1 }}>
            {records.map((r) => (
              <OutputRow
                key={r.output.key}
                record={r}
                busy={busy}
                onSetLimit={onSetLimit}
                onForget={onForget}
              />
            ))}
          </Stack>
        )}
      </CardContent>
    </Card>
  );
}
