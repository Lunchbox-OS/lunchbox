// The sign-in screen (issue #156).
//
// Three shapes, chosen by what the device says about itself:
//
// - **Setup** — no password yet. Asks for the six digits on the television and
//   a password to set. This is the only moment the device's own screen is a
//   trusted channel: the parent is holding the device and the child has not
//   met it.
// - **Password** — the ordinary case.
// - **Approve on your phone** — offered only when a companion is paired.
//   Shows six digits and waits; the parent compares them against the app and
//   taps approve. Nothing secret crosses the network in either direction.
//
// The password form stays available even when approval is offered, because the
// phone can be flat, lost, or in another building.

import { useCallback, useEffect, useRef, useState } from "react";
import Alert from "@mui/material/Alert";
import Box from "@mui/material/Box";
import Button from "@mui/material/Button";
import Card from "@mui/material/Card";
import CardContent from "@mui/material/CardContent";
import CircularProgress from "@mui/material/CircularProgress";
import Divider from "@mui/material/Divider";
import Stack from "@mui/material/Stack";
import TextField from "@mui/material/TextField";
import Typography from "@mui/material/Typography";
import PhonelinkLockIcon from "@mui/icons-material/PhonelinkLock";
import {
  AuthError,
  completeSetup,
  login,
  pollApproval,
  requestApproval,
  type WebAuthStatus,
} from "../api/auth";

/** How often to ask whether the phone has approved yet. */
const POLL_MS = 2000;

export function LoginPage({
  status,
  onSignedIn,
}: {
  status: WebAuthStatus;
  onSignedIn: () => void;
}) {
  return (
    <Box
      sx={{
        minHeight: "100dvh",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        p: 2,
      }}
    >
      <Card sx={{ width: "100%", maxWidth: 420 }}>
        <CardContent>
          <Typography variant="h5" color="primary" sx={{ fontWeight: 700, mb: 0.5 }}>
            Lunchbox
          </Typography>
          {status.configured ? (
            <SignIn status={status} onSignedIn={onSignedIn} />
          ) : (
            <FirstRunSetup onSignedIn={onSignedIn} />
          )}
        </CardContent>
      </Card>
    </Box>
  );
}

// ---------------------------------------------------------------------------
// First run
// ---------------------------------------------------------------------------

function FirstRunSetup({ onSignedIn }: { onSignedIn: () => void }) {
  const [code, setCode] = useState("");
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const mismatch = confirm.length > 0 && password !== confirm;

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (mismatch) return;
    setBusy(true);
    setError(null);
    try {
      await completeSetup(code.trim(), password);
      onSignedIn();
    } catch (err) {
      setError(err instanceof AuthError ? err.message : String(err));
      setBusy(false);
    }
  };

  return (
    <form onSubmit={submit}>
      <Typography variant="body2" color="text.secondary" sx={{ mb: 2 }}>
        This device has no management password yet. Enter the setup code shown
        on its screen, and choose a password.
      </Typography>
      <Stack spacing={2}>
        {error && <Alert severity="error">{error}</Alert>}
        <TextField
          label="Setup code"
          value={code}
          onChange={(e) => setCode(e.target.value)}
          autoFocus
          fullWidth
          slotProps={{
            htmlInput: {
              inputMode: "numeric",
              autoComplete: "one-time-code",
              maxLength: 6,
            },
          }}
        />
        <TextField
          label="New password"
          type="password"
          value={password}
          onChange={(e) => setPassword(e.target.value)}
          helperText="At least 8 characters"
          fullWidth
          slotProps={{ htmlInput: { autoComplete: "new-password" } }}
        />
        <TextField
          label="Confirm password"
          type="password"
          value={confirm}
          onChange={(e) => setConfirm(e.target.value)}
          error={mismatch}
          helperText={mismatch ? "The passwords do not match" : " "}
          fullWidth
          slotProps={{ htmlInput: { autoComplete: "new-password" } }}
        />
        <Button
          type="submit"
          variant="contained"
          disabled={busy || !code || password.length < 8 || mismatch}
        >
          {busy ? "Setting up…" : "Set password"}
        </Button>
      </Stack>
    </form>
  );
}

// ---------------------------------------------------------------------------
// Signing in
// ---------------------------------------------------------------------------

function SignIn({
  status,
  onSignedIn,
}: {
  status: WebAuthStatus;
  onSignedIn: () => void;
}) {
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await login(password);
      onSignedIn();
    } catch (err) {
      setError(
        err instanceof AuthError && err.retryAfter !== undefined
          ? `Too many attempts. Try again in ${formatWait(err.retryAfter)}.`
          : err instanceof AuthError
            ? err.message
            : String(err),
      );
      setBusy(false);
    }
  };

  return (
    <>
      <form onSubmit={submit}>
        <Typography variant="body2" color="text.secondary" sx={{ mb: 2 }}>
          Sign in to manage this device.
        </Typography>
        <Stack spacing={2}>
          {error && <Alert severity="error">{error}</Alert>}
          <TextField
            label="Password"
            type="password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            autoFocus
            fullWidth
            slotProps={{ htmlInput: { autoComplete: "current-password" } }}
          />
          <Button type="submit" variant="contained" disabled={busy || !password}>
            {busy ? "Signing in…" : "Sign in"}
          </Button>
        </Stack>
      </form>

      {status.companion_available && (
        <>
          <Divider sx={{ my: 3 }}>or</Divider>
          <CompanionApproval onSignedIn={onSignedIn} />
        </>
      )}
    </>
  );
}

// ---------------------------------------------------------------------------
// Approval on the paired phone
// ---------------------------------------------------------------------------

function CompanionApproval({ onSignedIn }: { onSignedIn: () => void }) {
  const [pending, setPending] = useState<{ code: string; pollToken: string } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // Held in a ref as well so the polling effect does not restart on rerender.
  const signedIn = useRef(false);

  const start = async () => {
    setBusy(true);
    setError(null);
    try {
      const requested = await requestApproval();
      setPending({ code: requested.code, pollToken: requested.poll_token });
    } catch (err) {
      setError(err instanceof AuthError ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  };

  const stop = useCallback(() => setPending(null), []);

  useEffect(() => {
    if (!pending) return;
    let cancelled = false;
    const timer = setInterval(async () => {
      try {
        const answer = await pollApproval(pending.pollToken);
        if (cancelled) return;
        switch (answer.state) {
          case "approved":
            if (!signedIn.current) {
              signedIn.current = true;
              onSignedIn();
            }
            break;
          case "denied":
            setError("The request was declined on the phone.");
            stop();
            break;
          case "expired":
            setError("The request timed out. Try again.");
            stop();
            break;
          default:
            break;
        }
      } catch {
        // A poll that fails is not worth surfacing: the next one is two
        // seconds away, and the request expires on its own.
      }
    }, POLL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [pending, onSignedIn, stop]);

  if (pending) {
    return (
      <Stack spacing={2} sx={{ alignItems: "center" }}>
        <Typography variant="body2" color="text.secondary" align="center">
          Open the Lunchbox app on your phone and approve this sign-in. Check
          that it shows the same number:
        </Typography>
        <Typography
          sx={{
            fontFamily: "monospace",
            fontSize: 44,
            fontWeight: 700,
            letterSpacing: 6,
          }}
        >
          {pending.code}
        </Typography>
        <CircularProgress size={20} />
        <Button variant="text" onClick={stop}>
          Cancel
        </Button>
      </Stack>
    );
  }

  return (
    <Stack spacing={1}>
      {error && <Alert severity="warning">{error}</Alert>}
      <Button
        variant="outlined"
        startIcon={<PhonelinkLockIcon />}
        onClick={start}
        disabled={busy}
      >
        {busy ? "Asking…" : "Approve on my phone"}
      </Button>
    </Stack>
  );
}

function formatWait(seconds: number): string {
  if (seconds < 60) return `${seconds} seconds`;
  const minutes = Math.ceil(seconds / 60);
  return `${minutes} minute${minutes === 1 ? "" : "s"}`;
}
