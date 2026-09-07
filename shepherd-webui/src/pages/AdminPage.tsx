import { useState } from "react";
import Alert from "@mui/material/Alert";
import Box from "@mui/material/Box";
import Button from "@mui/material/Button";
import Card from "@mui/material/Card";
import CardContent from "@mui/material/CardContent";
import Slider from "@mui/material/Slider";
import Snackbar from "@mui/material/Snackbar";
import Stack from "@mui/material/Stack";
import Switch from "@mui/material/Switch";
import FormControlLabel from "@mui/material/FormControlLabel";
import Typography from "@mui/material/Typography";
import LogoutIcon from "@mui/icons-material/Logout";
import RefreshIcon from "@mui/icons-material/Refresh";
import VolumeOffIcon from "@mui/icons-material/VolumeOff";
import VolumeDownIcon from "@mui/icons-material/VolumeDown";
import VolumeUpIcon from "@mui/icons-material/VolumeUp";
import BrightnessHighIcon from "@mui/icons-material/BrightnessHigh";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import {
  forgetAudioOutput,
  selectAudioOutput,
  getBrightness,
  getVolume,
  listAudioOutputs,
  logoutUser,
  refreshMedia,
  reloadConfig,
  setAudioOutputLimits,
  setAutoBrightness,
  setBrightnessPercent,
  setVolumeMuted,
  setVolumePercent,
} from "../api/client";
import { AudioOutputsCard } from "../components/AudioOutputsCard";
import { useEvents } from "../hooks/useEvents";
import { Spinner } from "../components/Spinner";
import { ConnectionSettings } from "./ConnectionSettings";
import { SessionsCard } from "../components/SessionsCard";

export function AdminPage() {
  const queryClient = useQueryClient();
  const { data: volume, isPending: volLoading } = useQuery({
    queryKey: ["volume"],
    queryFn: getVolume,
  });
  const { data: brightness, isPending: brightLoading } = useQuery({
    queryKey: ["brightness"],
    queryFn: getBrightness,
  });
  const { data: audioOutputs } = useQuery({
    queryKey: ["audio-outputs"],
    queryFn: listAudioOutputs,
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

  const setOutputLimitMutation = useMutation({
    mutationFn: ({ key, max }: { key: string; max: number | null }) =>
      setAudioOutputLimits(key, max),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["audio-outputs"] });
      queryClient.invalidateQueries({ queryKey: ["volume"] });
      flash("Limit saved");
    },
    onError: (e) => flash(String(e), false),
  });

  const selectOutputMutation = useMutation({
    mutationFn: selectAudioOutput,
    onSuccess: (v) => {
      queryClient.invalidateQueries({ queryKey: ["audio-outputs"] });
      queryClient.invalidateQueries({ queryKey: ["volume"] });
      // Naming the device confirms the switch landed where the parent meant,
      // which matters because the daemon can refuse one it can no longer see.
      flash(`Now playing through ${v.output?.description ?? "the chosen device"}`);
    },
    onError: (e) => flash(String(e), false),
  });

  const forgetOutputMutation = useMutation({
    mutationFn: forgetAudioOutput,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["audio-outputs"] });
      queryClient.invalidateQueries({ queryKey: ["volume"] });
      flash("Device forgotten");
    },
    onError: (e) => flash(String(e), false),
  });

  const setMutedMutation = useMutation({
    mutationFn: setVolumeMuted,
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["volume"] }),
    onError: (e) => flash(String(e), false),
  });

  const setBrightnessMutation = useMutation({
    mutationFn: setBrightnessPercent,
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["brightness"] }),
    onError: (e) => flash(String(e), false),
  });

  const setAutoBrightnessMutation = useMutation({
    mutationFn: setAutoBrightness,
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["brightness"] }),
    onError: (e) => flash(String(e), false),
  });

  const reloadMutation = useMutation({
    mutationFn: reloadConfig,
    onSuccess: (res) => flash(`Config reloaded (${res.entry_count} entries)`),
    onError: (e) => flash(String(e), false),
  });

  const refreshMediaMutation = useMutation({
    mutationFn: refreshMedia,
    // Deliberately not "done": the daemon has accepted the request, and the
    // playlist fetches and downloads it kicks off outlive this response by
    // minutes. A refresh that could not reach what it went for reports itself
    // on the Device health page.
    onSuccess: () => flash("Refreshing media libraries…"),
    onError: (e) => flash(String(e), false),
  });

  const logoutMutation = useMutation({
    mutationFn: logoutUser,
    onSuccess: () => flash("Logout requested"),
    onError: (e) => flash(String(e), false),
  });

  const busyVol = setPercentMutation.isPending || setMutedMutation.isPending;
  const busyBright =
    setBrightnessMutation.isPending || setAutoBrightnessMutation.isPending;

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
              {/* Name the output the reading applies to. The active output can
                  change with no user action (a headset is plugged in, a dock
                  switches sinks), so without this the volume appears to move on
                  its own. */}
              {volume.output && (
                <Typography variant="caption" color="text.secondary">
                  Output: {volume.output.description || volume.output.key}
                </Typography>
              )}
              {volume.backend && (
                <Typography variant="caption" color="text.disabled">Backend: {volume.backend}</Typography>
              )}
            </Stack>
          ) : null}
        </CardContent>
      </Card>

      {/* Per-device volume limits (issue #124) */}
      <AudioOutputsCard
        records={audioOutputs}
        busy={
          setOutputLimitMutation.isPending ||
          forgetOutputMutation.isPending ||
          selectOutputMutation.isPending
        }
        onSetLimit={(key, max) => setOutputLimitMutation.mutate({ key, max })}
        onForget={(key) => forgetOutputMutation.mutate(key)}
        onSelect={(key) => selectOutputMutation.mutate(key)}
      />

      {/* Brightness */}
      <Card variant="outlined">
        <CardContent>
          <Typography variant="subtitle1" gutterBottom sx={{ fontWeight: 600 }}>Brightness</Typography>
          {brightLoading && !brightness ? (
            <Box sx={{ display: "flex", justifyContent: "center", py: 2 }}><Spinner /></Box>
          ) : brightness && !brightness.available ? (
            <Typography variant="body2" color="text.secondary">
              Brightness control not available on this device
            </Typography>
          ) : brightness ? (
            <Stack spacing={1}>
              <Box sx={{ display: "flex", alignItems: "center", gap: 2 }}>
                <BrightnessHighIcon fontSize="small" color="action" />
                <Slider
                  min={brightness.restrictions.min_brightness ?? 0}
                  max={brightness.restrictions.max_brightness ?? 100}
                  value={brightness.percent}
                  disabled={busyBright || !brightness.restrictions.allow_change}
                  onChange={(_, v) => setBrightnessMutation.mutate(v as number)}
                  aria-label="Brightness"
                  sx={{ flex: 1 }}
                />
                <Typography variant="body2" color="text.secondary" sx={{ minWidth: 40, textAlign: "right" }}>
                  {`${brightness.percent}%`}
                </Typography>
              </Box>
              {brightness.auto_available && (
                <FormControlLabel
                  control={
                    <Switch
                      checked={brightness.auto_enabled}
                      disabled={busyBright}
                      onChange={(_, checked) => setAutoBrightnessMutation.mutate(checked)}
                    />
                  }
                  label="Automatic brightness"
                />
              )}
              {brightness.backend && (
                <Typography variant="caption" color="text.disabled">Backend: {brightness.backend}</Typography>
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

      {/* Media */}
      <Card variant="outlined">
        <CardContent>
          <Typography variant="subtitle1" gutterBottom sx={{ fontWeight: 600 }}>Media</Typography>
          <Typography variant="body2" color="text.secondary" sx={{ mb: 1.5 }}>
            Fetch playlists, videos and sponsor segments now, instead of waiting for
            their caches to expire. Use this after adding something to a playlist.
          </Typography>
          <Button
            variant="outlined"
            onClick={() => refreshMediaMutation.mutate()}
            disabled={refreshMediaMutation.isPending}
            startIcon={refreshMediaMutation.isPending ? <Spinner size={16} /> : <RefreshIcon />}
          >
            Refresh Media
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

      {/* Signed-in browsers (issue #156) */}
      <SessionsCard />

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
