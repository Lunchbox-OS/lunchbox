// Live browser sessions, and the button that ends them (issue #156).
//
// The answer to the thing the old bearer token could not do: a credential that
// was typed into one browser and never expired could only be withdrawn by
// factory-resetting the device, which also unpaired the phone. Here a lost
// laptop is one row and one click.

import { useState } from "react";
import Alert from "@mui/material/Alert";
import Box from "@mui/material/Box";
import Button from "@mui/material/Button";
import Card from "@mui/material/Card";
import CardContent from "@mui/material/CardContent";
import Chip from "@mui/material/Chip";
import Dialog from "@mui/material/Dialog";
import DialogActions from "@mui/material/DialogActions";
import DialogContent from "@mui/material/DialogContent";
import DialogContentText from "@mui/material/DialogContentText";
import DialogTitle from "@mui/material/DialogTitle";
import IconButton from "@mui/material/IconButton";
import List from "@mui/material/List";
import ListItem from "@mui/material/ListItem";
import ListItemText from "@mui/material/ListItemText";
import Stack from "@mui/material/Stack";
import Typography from "@mui/material/Typography";
import DeleteIcon from "@mui/icons-material/Delete";
import LogoutIcon from "@mui/icons-material/Logout";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { listSessions, revokeSession, signOut, type WebSessionInfo } from "../api/auth";
import { Spinner } from "./Spinner";

export function SessionsCard() {
  const queryClient = useQueryClient();
  const { data: sessions, isPending } = useQuery({
    queryKey: ["web-sessions"],
    queryFn: listSessions,
  });
  const [confirming, setConfirming] = useState<WebSessionInfo | null>(null);
  const [error, setError] = useState<string | null>(null);

  const revoke = useMutation({
    mutationFn: revokeSession,
    onSuccess: (_, id) => {
      const wasCurrent = sessions?.find((s) => s.id === id)?.current;
      if (wasCurrent) {
        // Revoking your own session leaves this tab holding a dead cookie;
        // the next request would 401 and bounce to the login page anyway, so
        // go there deliberately instead of letting it happen by accident.
        window.location.reload();
        return;
      }
      void queryClient.invalidateQueries({ queryKey: ["web-sessions"] });
    },
    onError: (e: Error) => setError(e.message),
  });

  const doSignOut = useMutation({
    mutationFn: signOut,
    onSuccess: () => window.location.reload(),
    onError: (e: Error) => setError(e.message),
  });

  return (
    <Card>
      <CardContent>
        <Typography variant="subtitle1" sx={{ fontWeight: 600, mb: 1 }}>
          Signed-in browsers
        </Typography>
        {error && (
          <Alert severity="error" sx={{ mb: 2 }} onClose={() => setError(null)}>
            {error}
          </Alert>
        )}
        {isPending ? (
          <Spinner />
        ) : !sessions?.length ? (
          <Typography variant="body2" color="text.secondary">
            No sessions.
          </Typography>
        ) : (
          <List dense disablePadding>
            {sessions.map((s) => (
              <ListItem
                key={s.id}
                disableGutters
                secondaryAction={
                  <IconButton
                    edge="end"
                    aria-label={`Sign out ${s.label}`}
                    onClick={() => setConfirming(s)}
                  >
                    <DeleteIcon fontSize="small" />
                  </IconButton>
                }
              >
                <ListItemText
                  primary={
                    <Box sx={{ display: "flex", alignItems: "center", gap: 1 }}>
                      <span>{s.label}</span>
                      {s.current && <Chip size="small" label="this browser" />}
                    </Box>
                  }
                  secondary={`${s.peer} · last used ${relative(s.last_seen)}`}
                />
              </ListItem>
            ))}
          </List>
        )}

        <Stack direction="row" spacing={1} sx={{ mt: 2 }}>
          <Button
            size="small"
            variant="outlined"
            startIcon={<LogoutIcon />}
            onClick={() => doSignOut.mutate()}
            disabled={doSignOut.isPending}
          >
            Sign out
          </Button>
        </Stack>
      </CardContent>

      <Dialog open={confirming !== null} onClose={() => setConfirming(null)}>
        <DialogTitle>End this session?</DialogTitle>
        <DialogContent>
          <DialogContentText>
            {confirming?.current
              ? "This is the browser you are using. You will be signed out."
              : `“${confirming?.label}” will have to sign in again.`}
          </DialogContentText>
        </DialogContent>
        <DialogActions>
          <Button onClick={() => setConfirming(null)}>Cancel</Button>
          <Button
            color="error"
            onClick={() => {
              if (confirming) revoke.mutate(confirming.id);
              setConfirming(null);
            }}
          >
            End session
          </Button>
        </DialogActions>
      </Dialog>
    </Card>
  );
}

/** "3 minutes ago", roughly. Precision past that is not what this is for. */
function relative(iso: string): string {
  const then = new Date(iso).getTime();
  if (Number.isNaN(then)) return "unknown";
  const seconds = Math.max(0, Math.round((Date.now() - then) / 1000));
  if (seconds < 90) return "just now";
  const minutes = Math.round(seconds / 60);
  if (minutes < 90) return `${minutes} minutes ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 36) return `${hours} hours ago`;
  return `${Math.round(hours / 24)} days ago`;
}
