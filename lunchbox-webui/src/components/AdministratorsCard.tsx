// The device's paired phones, and the ones asking to be (issue #149).
//
// A device is claimed by the first phone that reaches it, because there is
// nobody to ask yet. Every phone after that arrives here as a request showing
// six digits; whoever is already trusted compares them against that phone's
// screen and taps. The comparison is the security property — a phone racing
// the one you meant carries a different number.
//
// This exists in the browser as well as on the companion because the parent
// who is holding the device when a second phone asks is at least as likely to
// be sitting at a laptop, and leaving approval to the phone alone would have
// meant the only way to add a caregiver was to find whoever already had one.

import { useState } from "react";
import Alert from "@mui/material/Alert";
import Box from "@mui/material/Box";
import Button from "@mui/material/Button";
import Card from "@mui/material/Card";
import CardContent from "@mui/material/CardContent";
import Dialog from "@mui/material/Dialog";
import DialogActions from "@mui/material/DialogActions";
import DialogContent from "@mui/material/DialogContent";
import DialogContentText from "@mui/material/DialogContentText";
import DialogTitle from "@mui/material/DialogTitle";
import List from "@mui/material/List";
import ListItem from "@mui/material/ListItem";
import ListItemText from "@mui/material/ListItemText";
import Stack from "@mui/material/Stack";
import Typography from "@mui/material/Typography";
import DeleteIcon from "@mui/icons-material/Delete";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  approveEnrolmentRequest,
  denyEnrolmentRequest,
  listAdmins,
  listEnrolmentRequests,
  revokeAdmin,
} from "../api/client";
import type { AdminSummary } from "../api/wire-types.generated";
import { Spinner } from "./Spinner";

/**
 * How often to ask whether a phone is waiting.
 *
 * The same few seconds the companion's own screen uses: somebody is standing
 * there holding a phone that says "waiting for approval", and a slow poll
 * reads as the feature being broken.
 */
const POLL_INTERVAL_MS = 3000;

export function AdministratorsCard() {
  const queryClient = useQueryClient();
  // Both on the same poll: a phone approved from the *companion* changes the
  // roster without this browser doing anything, and a list that only refreshed
  // on its own mutations would quietly disagree with the device.
  const { data: admins, isPending, error: loadError } = useQuery({
    queryKey: ["admins"],
    queryFn: listAdmins,
    refetchInterval: POLL_INTERVAL_MS,
  });
  const { data: requests } = useQuery({
    queryKey: ["enrolment-requests"],
    queryFn: listEnrolmentRequests,
    refetchInterval: POLL_INTERVAL_MS,
  });
  const [confirming, setConfirming] = useState<AdminSummary | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = () => {
    void queryClient.invalidateQueries({ queryKey: ["admins"] });
    void queryClient.invalidateQueries({ queryKey: ["enrolment-requests"] });
  };

  const approve = useMutation({
    mutationFn: approveEnrolmentRequest,
    onSuccess: refresh,
    onError: (e: Error) => setError(e.message),
  });
  const deny = useMutation({
    mutationFn: denyEnrolmentRequest,
    onSuccess: refresh,
    onError: (e: Error) => setError(e.message),
  });
  const revoke = useMutation({
    mutationFn: revokeAdmin,
    onSuccess: refresh,
    onError: (e: Error) => setError(e.message),
  });

  // A device with Bluetooth management switched off has no roster to show and
  // says so rather than answering with an empty list. Nothing here applies, so
  // render nothing rather than an empty card that looks like a fault.
  if (loadError) return null;

  return (
    <Card>
      <CardContent>
        <Typography variant="subtitle1" sx={{ fontWeight: 600, mb: 1 }}>
          Administrators
        </Typography>

        {error && (
          <Alert severity="error" sx={{ mb: 2 }} onClose={() => setError(null)}>
            {error}
          </Alert>
        )}

        {requests?.map((request) => (
          <Alert
            key={request.id}
            severity="info"
            sx={{ mb: 2 }}
            action={
              <Stack direction="row" spacing={1}>
                <Button
                  size="small"
                  onClick={() => deny.mutate(request.id)}
                  disabled={deny.isPending || approve.isPending}
                >
                  Not mine
                </Button>
                <Button
                  size="small"
                  variant="contained"
                  onClick={() => approve.mutate(request.id)}
                  disabled={deny.isPending || approve.isPending}
                >
                  Approve
                </Button>
              </Stack>
            }
          >
            <Typography variant="body2" sx={{ fontWeight: 600 }}>
              {request.device_name} wants to administer this device
            </Typography>
            <Typography
              variant="h5"
              sx={{ fontFamily: "monospace", fontWeight: 700, my: 0.5 }}
            >
              {request.code}
            </Typography>
            <Typography variant="caption" color="text.secondary">
              Only approve if that phone is showing the same number · {request.peer}
            </Typography>
          </Alert>
        ))}

        {isPending ? (
          <Spinner />
        ) : (
          <List dense disablePadding>
            {admins?.map((admin) => (
              <ListItem
                key={admin.id}
                disableGutters
                secondaryAction={
                  // The device refuses to remove the last administrator — that
                  // would leave it with nobody able to reach it and a phone
                  // still bonded to it. Don't offer a button whose only
                  // outcome is an error.
                  (admins?.length ?? 0) > 1 ? (
                    <Button
                      size="small"
                      color="error"
                      startIcon={<DeleteIcon />}
                      onClick={() => setConfirming(admin)}
                      disabled={revoke.isPending}
                    >
                      Remove
                    </Button>
                  ) : undefined
                }
              >
                <ListItemText
                  primary={admin.device_name}
                  secondary={
                    <Box component="span" sx={{ fontFamily: "monospace" }}>
                      {admin.identity_address}
                    </Box>
                  }
                />
              </ListItem>
            ))}
          </List>
        )}

        <Typography variant="caption" color="text.secondary">
          To add another phone, install the companion app on it and pair it with
          this device. It will appear here for you to approve.
        </Typography>
      </CardContent>

      <Dialog open={confirming !== null} onClose={() => setConfirming(null)}>
        <DialogTitle>Remove {confirming?.device_name}?</DialogTitle>
        <DialogContent>
          <DialogContentText>
            {confirming?.device_name} will lose access to this device and its
            Bluetooth bond will be removed. It can ask to be added again.
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
            Remove
          </Button>
        </DialogActions>
      </Dialog>
    </Card>
  );
}
