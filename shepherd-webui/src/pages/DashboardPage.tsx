import { useEffect, useState } from "react";
import Alert from "@mui/material/Alert";
import Box from "@mui/material/Box";
import Button from "@mui/material/Button";
import Card from "@mui/material/Card";
import CardContent from "@mui/material/CardContent";
import Grid from "@mui/material/Grid";
import Snackbar from "@mui/material/Snackbar";
import Typography from "@mui/material/Typography";
import StopIcon from "@mui/icons-material/Stop";
import { styled } from "@mui/material/styles";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { extendSession, getCurrentSession, stopSession } from "../api/client";
import { formatDuration, type SessionInfo } from "../api/types";
import { useEvents } from "../hooks/useEvents";
import { Spinner } from "../components/Spinner";

const TIME_ADJUSTMENTS = [
  { label: "−30m", secs: -30 * 60 },
  { label: "−10m", secs: -10 * 60 },
  { label: "+10m", secs: 10 * 60 },
  { label: "+30m", secs: 30 * 60 },
];

const CountdownText = styled(Typography)(({ theme }) => ({
  fontSize: "clamp(3rem, 18vw, 5rem)",
  fontWeight: 700,
  fontVariantNumeric: "tabular-nums",
  letterSpacing: "-0.04em",
  lineHeight: 1,
  [theme.breakpoints.up("sm")]: { fontSize: "4rem" },
}));

export function DashboardPage() {
  const queryClient = useQueryClient();
  const { data: session, isPending: loading } = useQuery({
    queryKey: ["session", "current"],
    queryFn: getCurrentSession,
  });
  const [msg, setMsg] = useState<{ text: string; ok: boolean } | null>(null);

  useEvents();

  const flash = (text: string, ok = true) => {
    setMsg({ text, ok });
    setTimeout(() => setMsg(null), 3000);
  };

  const stopMutation = useMutation({
    mutationFn: stopSession,
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["session"] });
      flash("Session stopped");
    },
    onError: (e) => flash(String(e), false),
  });

  const adjustMutation = useMutation({
    mutationFn: extendSession,
    onSuccess: (_, secs) => {
      queryClient.invalidateQueries({ queryKey: ["session"] });
      flash(secs > 0 ? `+${secs / 60}m added` : `${secs / 60}m removed`);
    },
    onError: (e) => flash(String(e), false),
  });

  const busy = stopMutation.isPending || adjustMutation.isPending;

  return (
    <Box sx={{ display: "flex", flexDirection: "column", gap: 2 }}>
      <Typography variant="h6" sx={{ fontWeight: 700 }}>Now Playing</Typography>

      <Snackbar open={!!msg} message={msg?.text} autoHideDuration={3000} onClose={() => setMsg(null)}>
        <Alert severity={msg?.ok ? "success" : "error"} onClose={() => setMsg(null)} sx={{ width: "100%" }}>
          {msg?.text}
        </Alert>
      </Snackbar>

      {loading && !session && (
        <Box sx={{ display: "flex", justifyContent: "center", py: 6 }}>
          <Spinner />
        </Box>
      )}

      {!loading && !session && (
        <Card variant="outlined" sx={{ textAlign: "center", py: 5 }}>
          <CardContent>
            <Typography variant="body1" color="text.secondary" sx={{ fontWeight: 600 }}>
              Nothing running
            </Typography>
            <Typography variant="body2" color="text.disabled" sx={{ mt: 0.75 }}>
              Open Activities to launch an activity
            </Typography>
          </CardContent>
        </Card>
      )}

      {session && (
        <SessionCard
          session={session}
          busy={busy}
          onStop={() => stopMutation.mutate()}
          onAdjust={(secs) => adjustMutation.mutate(secs)}
        />
      )}
    </Box>
  );
}

const STATE_COLOR: Record<string, "success" | "warning" | "error" | "primary" | "disabled"> = {
  running: "success",
  warned: "warning",
  expiring: "error",
  launching: "primary",
  stopping: "warning",
  ended: "disabled",
};

function SessionCard({
  session,
  busy,
  onStop,
  onAdjust,
}: {
  session: SessionInfo;
  busy: boolean;
  onStop: () => void;
  onAdjust: (secs: number) => void;
}) {
  const deadline = session.deadline ? new Date(session.deadline) : null;
  const remaining = useCountdown(deadline);
  const stateColor = STATE_COLOR[session.state] ?? "disabled";

  const countdownColor =
    remaining < 60 ? "error.main" : remaining < 300 ? "warning.main" : "text.primary";

  return (
    <Card>
      <CardContent sx={{ display: "flex", flexDirection: "column", gap: 1.5 }}>
        {/* State badge */}
        <Box sx={{ display: "flex", alignItems: "center", gap: 1 }}>
          <Box
            sx={{
              width: 8,
              height: 8,
              borderRadius: "50%",
              bgcolor: `${stateColor}.main`,
              flexShrink: 0,
            }}
          />
          <Typography
            variant="caption"
            color="text.secondary"
            sx={{ fontWeight: 600, letterSpacing: "0.05em", textTransform: "uppercase" }}
          >
            {session.state.charAt(0).toUpperCase() + session.state.slice(1)}
          </Typography>
        </Box>

        <Typography variant="h5" sx={{ fontWeight: 700, letterSpacing: "-0.02em" }}>
          {session.label}
        </Typography>

        <CountdownText sx={{ color: deadline ? countdownColor : "text.disabled" }}>
          {deadline ? formatDuration(remaining) : "∞"}
        </CountdownText>

        {deadline && (
          <Typography variant="caption" color="text.secondary">
            Until {deadline.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}
          </Typography>
        )}

        {/* Time adjustment grid */}
        <Grid container spacing={1} sx={{ mt: 0.5 }}>
          {TIME_ADJUSTMENTS.map((adj) => (
            <Grid key={adj.secs} size={3}>
              <Button
                variant="outlined"
                fullWidth
                disabled={busy}
                onClick={() => onAdjust(adj.secs)}
                sx={{ fontWeight: 600, py: 1 }}
              >
                {adj.label}
              </Button>
            </Grid>
          ))}
        </Grid>

        <Button
          variant="contained"
          color="error"
          size="large"
          fullWidth
          startIcon={busy ? <Spinner size={18} /> : <StopIcon />}
          disabled={busy}
          onClick={onStop}
          sx={{ mt: 0.5, minHeight: 52 }}
        >
          Stop Session
        </Button>
      </CardContent>
    </Card>
  );
}

function useCountdown(deadline: Date | null): number {
  const [remaining, setRemaining] = useState(() =>
    deadline ? Math.max(0, (deadline.getTime() - Date.now()) / 1000) : 0,
  );

  useEffect(() => {
    if (!deadline) return;
    const tick = () => setRemaining(Math.max(0, (deadline.getTime() - Date.now()) / 1000));
    tick();
    const id = setInterval(tick, 1000);
    return () => clearInterval(id);
  }, [deadline]);

  return remaining;
}
