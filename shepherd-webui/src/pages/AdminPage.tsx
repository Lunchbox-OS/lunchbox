import { useState } from "react";
import Alert from "@mui/material/Alert";
import Box from "@mui/material/Box";
import Button from "@mui/material/Button";
import Card from "@mui/material/Card";
import CardContent from "@mui/material/CardContent";
import Slider from "@mui/material/Slider";
import Snackbar from "@mui/material/Snackbar";
import Stack from "@mui/material/Stack";
import Typography from "@mui/material/Typography";
import VolumeOffIcon from "@mui/icons-material/VolumeOff";
import VolumeDownIcon from "@mui/icons-material/VolumeDown";
import VolumeUpIcon from "@mui/icons-material/VolumeUp";
import {
  enterMaintenance,
  exitMaintenance,
  getMaintenance,
  getVolume,
  reloadConfig,
  setVolumeMuted,
  setVolumePercent,
} from "../api/client";
import { useApi } from "../hooks/useApi";
import { useEvents } from "../hooks/useEvents";
import { Spinner } from "../components/Spinner";
import { ConnectionSettings } from "./ConnectionSettings";

export function AdminPage() {
  const { data: volume, loading: volLoading, refetch: refetchVolume } = useApi(getVolume, []);
  const { data: maintenance, loading: maintLoading, refetch: refetchMaint } = useApi(getMaintenance, []);

  const [busyVol, setBusyVol] = useState(false);
  const [busyMaint, setBusyMaint] = useState(false);
  const [busyReload, setBusyReload] = useState(false);
  const [msg, setMsg] = useState<{ text: string; ok: boolean } | null>(null);
  const [showConn, setShowConn] = useState(false);

  useEvents(() => { refetchVolume(); refetchMaint(); });

  const flash = (text: string, ok = true) => {
    setMsg({ text, ok });
    setTimeout(() => setMsg(null), 3000);
  };

  const handleSetVolume = async (percent: number) => {
    setBusyVol(true);
    try {
      await setVolumePercent(percent);
      refetchVolume();
    } catch (e) {
      flash(String(e), false);
    } finally {
      setBusyVol(false);
    }
  };

  const handleToggleMute = async () => {
    if (!volume) return;
    setBusyVol(true);
    try {
      await setVolumeMuted(!volume.muted);
      refetchVolume();
    } catch (e) {
      flash(String(e), false);
    } finally {
      setBusyVol(false);
    }
  };

  const handleMaintenance = async () => {
    setBusyMaint(true);
    try {
      if (maintenance?.active) {
        await exitMaintenance();
        flash("Maintenance mode disabled");
      } else {
        await enterMaintenance();
        flash("Maintenance mode enabled");
      }
      refetchMaint();
    } catch (e) {
      flash(String(e), false);
    } finally {
      setBusyMaint(false);
    }
  };

  const handleReload = async () => {
    setBusyReload(true);
    try {
      const res = await reloadConfig();
      flash(`Config reloaded (${res.entry_count} entries)`);
    } catch (e) {
      flash(String(e), false);
    } finally {
      setBusyReload(false);
    }
  };

  const VolumeIcon = !volume || volume.muted
    ? VolumeOffIcon
    : volume.percent < 50
    ? VolumeDownIcon
    : VolumeUpIcon;

  return (
    <Box sx={{ display: "flex", flexDirection: "column", gap: 2 }}>
      <Typography variant="h6" sx={{ fontWeight: 700 }}>Admin</Typography>

      <Snackbar open={!!msg} autoHideDuration={3000} onClose={() => setMsg(null)}>
        <Alert severity={msg?.ok ? "success" : "error"} onClose={() => setMsg(null)} sx={{ width: "100%" }}>
          {msg?.text}
        </Alert>
      </Snackbar>

      {/* Volume */}
      <Card variant="outlined">
        <CardContent>
          <Typography variant="subtitle1" gutterBottom sx={{ fontWeight: 600 }}>Volume</Typography>
          {volLoading && !volume ? (
            <Box sx={{ display: "flex", justifyContent: "center", py: 2 }}><Spinner /></Box>
          ) : volume && !volume.available ? (
            <Typography variant="body2" color="text.secondary">
              Volume control not available on this device
            </Typography>
          ) : volume ? (
            <Stack spacing={1}>
              <Box sx={{ display: "flex", alignItems: "center", gap: 2 }}>
                <Button
                  variant={volume.muted ? "contained" : "outlined"}
                  size="small"
                  onClick={handleToggleMute}
                  disabled={busyVol || !volume.restrictions.allow_mute}
                  sx={{ minWidth: 44, px: 1 }}
                  aria-label={volume.muted ? "Unmute" : "Mute"}
                >
                  <VolumeIcon fontSize="small" />
                </Button>
                <Slider
                  min={volume.restrictions.min_volume ?? 0}
                  max={volume.restrictions.max_volume ?? 100}
                  value={volume.muted ? 0 : volume.percent}
                  disabled={busyVol || !volume.restrictions.allow_change}
                  onChange={(_, v) => handleSetVolume(v as number)}
                  aria-label="Volume"
                  sx={{ flex: 1 }}
                />
                <Typography variant="body2" color="text.secondary" sx={{ minWidth: 40, textAlign: "right" }}>
                  {volume.muted ? "Muted" : `${volume.percent}%`}
                </Typography>
              </Box>
              {volume.backend && (
                <Typography variant="caption" color="text.disabled">Backend: {volume.backend}</Typography>
              )}
            </Stack>
          ) : null}
        </CardContent>
      </Card>

      {/* Maintenance Mode */}
      <Card variant="outlined">
        <CardContent>
          <Typography variant="subtitle1" gutterBottom sx={{ fontWeight: 600 }}>Maintenance Mode</Typography>
          <Typography variant="body2" color="text.secondary" sx={{ mb: 1.5 }}>
            Temporarily suspends compositor restrictions so you can access system settings.
            Resets automatically on restart.
          </Typography>
          {maintLoading && !maintenance ? (
            <Box sx={{ display: "flex", justifyContent: "center", py: 2 }}><Spinner /></Box>
          ) : (
            <Box sx={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
              <Box sx={{ display: "flex", alignItems: "center", gap: 1 }}>
                <Box
                  sx={{
                    width: 10,
                    height: 10,
                    borderRadius: "50%",
                    bgcolor: maintenance?.active ? "warning.main" : "action.disabled",
                  }}
                />
                <Typography variant="body2">
                  {maintenance?.active
                    ? `Active since ${new Date(maintenance.activated_at!).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}`
                    : "Inactive"}
                </Typography>
              </Box>
              <Button
                variant={maintenance?.active ? "contained" : "outlined"}
                color={maintenance?.active ? "warning" : "primary"}
                size="small"
                onClick={handleMaintenance}
                disabled={busyMaint}
              >
                {busyMaint ? <Spinner size={18} /> : maintenance?.active ? "Disable" : "Enable"}
              </Button>
            </Box>
          )}
        </CardContent>
      </Card>

      {/* Config */}
      <Card variant="outlined">
        <CardContent>
          <Typography variant="subtitle1" gutterBottom sx={{ fontWeight: 600 }}>Configuration</Typography>
          <Typography variant="body2" color="text.secondary" sx={{ mb: 1.5 }}>
            Reload the configuration file from disk without restarting the daemon.
          </Typography>
          <Button
            variant="outlined"
            onClick={handleReload}
            disabled={busyReload}
            startIcon={busyReload ? <Spinner size={16} /> : undefined}
          >
            Reload Config
          </Button>
        </CardContent>
      </Card>

      {/* Connection */}
      <Card variant="outlined">
        <CardContent sx={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
          <Box>
            <Typography variant="subtitle1" sx={{ fontWeight: 600 }}>Connection</Typography>
            <Typography variant="caption" color="text.secondary">
              {localStorage.getItem("apiBase") || "(same origin)"}
            </Typography>
          </Box>
          <Button variant="text" size="small" onClick={() => setShowConn(true)}>
            Edit
          </Button>
        </CardContent>
      </Card>

      {showConn && <ConnectionSettings onClose={() => setShowConn(false)} />}
    </Box>
  );
}
