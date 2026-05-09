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
import LogoutIcon from "@mui/icons-material/Logout";
import VolumeOffIcon from "@mui/icons-material/VolumeOff";
import VolumeDownIcon from "@mui/icons-material/VolumeDown";
import VolumeUpIcon from "@mui/icons-material/VolumeUp";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import {
  getVolume,
  logoutUser,
  reloadConfig,
  setVolumeMuted,
  setVolumePercent,
} from "../api/client";
import { useEvents } from "../hooks/useEvents";
import { Spinner } from "../components/Spinner";
import { ConnectionSettings } from "./ConnectionSettings";

export function AdminPage() {
  const queryClient = useQueryClient();
  const { data: volume, isPending: volLoading } = useQuery({
    queryKey: ["volume"],
    queryFn: getVolume,
  });

  const [msg, setMsg] = useState<{ text: string; ok: boolean } | null>(null);
  const [showConn, setShowConn] = useState(false);

  useEvents();

  const flash = (text: string, ok = true) => {
    setMsg({ text, ok });
    setTimeout(() => setMsg(null), 3000);
  };

  const setPercentMutation = useMutation({
    mutationFn: setVolumePercent,
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["volume"] }),
    onError: (e) => flash(String(e), false),
  });

  const setMutedMutation = useMutation({
    mutationFn: setVolumeMuted,
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["volume"] }),
    onError: (e) => flash(String(e), false),
  });

  const reloadMutation = useMutation({
    mutationFn: reloadConfig,
    onSuccess: (res) => flash(`Config reloaded (${res.entry_count} entries)`),
    onError: (e) => flash(String(e), false),
  });

  const logoutMutation = useMutation({
    mutationFn: logoutUser,
    onSuccess: () => flash("Logout requested"),
    onError: (e) => flash(String(e), false),
  });

  const busyVol = setPercentMutation.isPending || setMutedMutation.isPending;

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
                  onClick={() => setMutedMutation.mutate(!volume.muted)}
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
                  onChange={(_, v) => setPercentMutation.mutate(v as number)}
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

      {/* Config */}
      <Card variant="outlined">
        <CardContent>
          <Typography variant="subtitle1" gutterBottom sx={{ fontWeight: 600 }}>Configuration</Typography>
          <Typography variant="body2" color="text.secondary" sx={{ mb: 1.5 }}>
            Reload the configuration file from disk without restarting the daemon.
          </Typography>
          <Button
            variant="outlined"
            onClick={() => reloadMutation.mutate()}
            disabled={reloadMutation.isPending}
            startIcon={reloadMutation.isPending ? <Spinner size={16} /> : undefined}
          >
            Reload Config
          </Button>
        </CardContent>
      </Card>

      {/* Logout */}
      <Card variant="outlined">
        <CardContent>
          <Typography variant="subtitle1" gutterBottom sx={{ fontWeight: 600 }}>Log Out User</Typography>
          <Typography variant="body2" color="text.secondary" sx={{ mb: 1.5 }}>
            End the user's desktop session and return to the login screen.
          </Typography>
          <Button
            variant="outlined"
            color="error"
            onClick={() => logoutMutation.mutate()}
            disabled={logoutMutation.isPending}
            startIcon={logoutMutation.isPending ? <Spinner size={16} /> : <LogoutIcon />}
          >
            Log Out
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
