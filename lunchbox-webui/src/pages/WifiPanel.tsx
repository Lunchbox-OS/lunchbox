import { useState } from "react";
import Alert from "@mui/material/Alert";
import AlertTitle from "@mui/material/AlertTitle";
import Box from "@mui/material/Box";
import Button from "@mui/material/Button";
import Card from "@mui/material/Card";
import CardContent from "@mui/material/CardContent";
import Checkbox from "@mui/material/Checkbox";
import Chip from "@mui/material/Chip";
import CircularProgress from "@mui/material/CircularProgress";
import Dialog from "@mui/material/Dialog";
import DialogActions from "@mui/material/DialogActions";
import DialogContent from "@mui/material/DialogContent";
import DialogTitle from "@mui/material/DialogTitle";
import Divider from "@mui/material/Divider";
import FormControlLabel from "@mui/material/FormControlLabel";
import IconButton from "@mui/material/IconButton";
import MenuItem from "@mui/material/MenuItem";
import Stack from "@mui/material/Stack";
import TextField from "@mui/material/TextField";
import Tooltip from "@mui/material/Tooltip";
import Typography from "@mui/material/Typography";
import LockIcon from "@mui/icons-material/LockOutlined";
import LockOpenIcon from "@mui/icons-material/LockOpenOutlined";
import RefreshIcon from "@mui/icons-material/RefreshOutlined";
import SignalWifi1BarIcon from "@mui/icons-material/SignalWifi1Bar";
import SignalWifi2BarIcon from "@mui/icons-material/SignalWifi2Bar";
import SignalWifi3BarIcon from "@mui/icons-material/SignalWifi3Bar";
import SignalWifi4BarIcon from "@mui/icons-material/SignalWifi4Bar";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  connectWifiNetwork,
  forgetWifiNetwork,
  getSavedWifiNetworks,
  getWifiNetworks,
  saveWifiNetwork,
  scanWifi,
} from "../api/client";
import type {
  SavedWifiNetwork,
  WifiJoinState,
  WifiNetwork,
  WifiScanView,
  WifiSecurity,
} from "../api/types";

/**
 * How often to re-read the scan while somebody is looking at this panel.
 *
 * Faster than the rest of the network page's ten seconds because this is also
 * how a join reports its outcome — the daemon cannot return it from the call
 * that started it, since association plus DHCP was measured at 3 to 45
 * seconds. {@link JOINING_REFRESH_MS} tightens it further while one is in
 * flight.
 */
const REFRESH_MS = 6_000;
const JOINING_REFRESH_MS = 2_000;

/** What a person may pick when typing a network name in by hand. */
const MANUAL_SECURITY: { value: WifiSecurity; label: string }[] = [
  { value: "wpa_psk", label: "WPA/WPA2 Personal" },
  { value: "sae", label: "WPA3 Personal" },
  { value: "open", label: "None (open network)" },
];

const SECURITY_LABEL: Record<WifiSecurity, string> = {
  open: "Open",
  owe: "Enhanced Open",
  wpa_psk: "WPA2",
  sae: "WPA3",
  enterprise: "Enterprise",
  wep: "WEP",
};

/** Whether this kind of network can be joined from here at all. */
function joinable(security: WifiSecurity): boolean {
  return security !== "enterprise" && security !== "wep";
}

function needsPassword(security: WifiSecurity): boolean {
  return security === "wpa_psk" || security === "sae";
}

/** Four bars, because a percentage means nothing to the person reading it. */
function SignalIcon({ percent }: { percent: number }) {
  const Icon =
    percent >= 75
      ? SignalWifi4BarIcon
      : percent >= 50
        ? SignalWifi3BarIcon
        : percent >= 25
          ? SignalWifi2BarIcon
          : SignalWifi1BarIcon;
  return (
    <Tooltip title={`Signal ${percent}%`}>
      <Icon fontSize="small" color="action" />
    </Tooltip>
  );
}

/**
 * What a failed join means, in a sentence a parent can act on.
 *
 * The distinction between the first two is the point of the whole reason
 * mapping: a device that associated and then got no address has a working
 * password and a broken router, and telling somebody "wrong password" there
 * has them retyping a correct one until they give up.
 */
function joinFailureText(state: WifiJoinState): string | null {
  if (state.state !== "failed") return null;
  switch (state.reason.kind) {
    case "wrong_password":
      return `The password for ${state.ssid} was refused. Check it and try again.`;
    case "no_address":
      return `${state.ssid} accepted the password but never gave this device an address. Check the router's DHCP.`;
    case "not_found":
      return `No network called ${state.ssid} answered. It may be out of range, switched off, or hidden — a hidden network has to be added by hand.`;
    case "not_authorized":
      return "This device is not allowed to change Wi-Fi settings.";
    case "rejected":
      return state.reason.detail ?? `${state.ssid} was refused.`;
    default:
      return state.reason.detail
        ? `Could not join ${state.ssid}: ${state.reason.detail}`
        : `Could not join ${state.ssid}.`;
  }
}

/** The form for joining one network, in a dialog. */
function JoinDialog({
  network,
  open,
  busy,
  onClose,
  onSubmit,
}: {
  network: WifiNetwork | null;
  open: boolean;
  busy: boolean;
  onClose: () => void;
  onSubmit: (password: string | null, connect: boolean) => void;
}) {
  const [password, setPassword] = useState("");
  const [confirmingConnect, setConfirmingConnect] = useState(false);

  if (!network) return null;
  const wantsPassword = needsPassword(network.security);

  const submit = (connect: boolean) => {
    onSubmit(wantsPassword ? password : null, connect);
    setPassword("");
    setConfirmingConnect(false);
  };

  return (
    <Dialog open={open} onClose={onClose} fullWidth maxWidth="xs">
      <DialogTitle>{network.ssid}</DialogTitle>
      <DialogContent>
        <Stack spacing={2} sx={{ pt: 1 }}>
          <Typography variant="body2" color="text.secondary">
            {SECURITY_LABEL[network.security]}
            {network.bands_ghz.length > 0 &&
              ` · ${network.bands_ghz.map((b) => `${b} GHz`).join(", ")}`}
          </Typography>
          {wantsPassword && (
            <TextField
              autoFocus
              fullWidth
              type="password"
              label="Password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              helperText="8 to 63 characters, or 64 hexadecimal digits."
              slotProps={{
                htmlInput: {
                  "aria-label": "Network password",
                  autoComplete: "new-password",
                },
              }}
            />
          )}
          {confirmingConnect ? (
            <Alert severity="warning">
              <AlertTitle>This page may stop responding</AlertTitle>
              This device will leave its current network to join {network.ssid}.
              If you reached this page over Wi-Fi, it will stop loading — the
              device's new address is on the companion app's Network screen.
            </Alert>
          ) : (
            <Typography variant="body2" color="text.secondary">
              Saving a network lets this device join it on its own later.
              Connecting now changes the network immediately.
            </Typography>
          )}
        </Stack>
      </DialogContent>
      <DialogActions>
        <Button onClick={onClose} disabled={busy}>
          Cancel
        </Button>
        {/*
          "Save for later" leads, and is the default action, because joining
          from a browser is the one action on this page that can cut the
          browser off.
        */}
        <Button
          variant="contained"
          onClick={() => submit(false)}
          disabled={busy || (wantsPassword && password.length === 0)}
        >
          Save for later
        </Button>
        {confirmingConnect ? (
          <Button
            color="warning"
            variant="contained"
            onClick={() => submit(true)}
            disabled={busy}
          >
            Connect anyway
          </Button>
        ) : (
          <Button
            onClick={() => setConfirmingConnect(true)}
            disabled={busy || (wantsPassword && password.length === 0)}
          >
            Connect now
          </Button>
        )}
      </DialogActions>
    </Dialog>
  );
}

/** The manual-entry form, for a network that does not broadcast its name. */
function ManualDialog({
  open,
  busy,
  onClose,
  onSubmit,
}: {
  open: boolean;
  busy: boolean;
  onClose: () => void;
  onSubmit: (
    ssid: string,
    security: WifiSecurity,
    password: string | null,
    hidden: boolean,
  ) => void;
}) {
  const [ssid, setSsid] = useState("");
  const [security, setSecurity] = useState<WifiSecurity>("wpa_psk");
  const [password, setPassword] = useState("");
  const [hidden, setHidden] = useState(true);

  const wantsPassword = needsPassword(security);
  const ready =
    ssid.trim().length > 0 && (!wantsPassword || password.length > 0);

  return (
    <Dialog open={open} onClose={onClose} fullWidth maxWidth="xs">
      <DialogTitle>Add a network</DialogTitle>
      <DialogContent>
        <Stack spacing={2} sx={{ pt: 1 }}>
          <TextField
            autoFocus
            fullWidth
            label="Network name"
            value={ssid}
            onChange={(e) => setSsid(e.target.value)}
            helperText="Exactly as the network announces it — names are case-sensitive."
            slotProps={{
              htmlInput: {
                "aria-label": "Network name",
                autoComplete: "off",
                spellCheck: false,
              },
            }}
          />
          <TextField
            select
            fullWidth
            label="Security"
            value={security}
            onChange={(e) => setSecurity(e.target.value as WifiSecurity)}
          >
            {MANUAL_SECURITY.map((option) => (
              <MenuItem key={option.value} value={option.value}>
                {option.label}
              </MenuItem>
            ))}
          </TextField>
          {wantsPassword && (
            <TextField
              fullWidth
              type="password"
              label="Password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              slotProps={{
                htmlInput: {
                  "aria-label": "Network password",
                  autoComplete: "new-password",
                },
              }}
            />
          )}
          <FormControlLabel
            control={
              <Checkbox
                checked={hidden}
                onChange={(e) => setHidden(e.target.checked)}
              />
            }
            label="This network does not broadcast its name"
          />
          {hidden && (
            <Typography variant="body2" color="text.secondary">
              The device will look for this network by name. Leave this unticked
              for an ordinary network you are adding ahead of time.
            </Typography>
          )}
        </Stack>
      </DialogContent>
      <DialogActions>
        <Button onClick={onClose} disabled={busy}>
          Cancel
        </Button>
        <Button
          variant="contained"
          disabled={busy || !ready}
          onClick={() => {
            onSubmit(
              ssid.trim(),
              security,
              wantsPassword ? password : null,
              hidden,
            );
            setSsid("");
            setPassword("");
          }}
        >
          Save for later
        </Button>
      </DialogActions>
    </Dialog>
  );
}

function SavedRow({
  network,
  busy,
  onConnect,
  onForget,
}: {
  network: SavedWifiNetwork;
  busy: boolean;
  onConnect: () => void;
  onForget: () => void;
}) {
  return (
    <Stack
      direction="row"
      spacing={1}
      sx={{ alignItems: "center", flexWrap: "wrap", rowGap: 0.5 }}
    >
      <Typography variant="body2" sx={{ flexGrow: 1 }}>
        {network.ssid}
      </Typography>
      {network.active && (
        <Chip size="small" color="success" label="Connected" />
      )}
      {network.hidden && (
        <Chip size="small" variant="outlined" label="Hidden" />
      )}
      <Typography variant="caption" color="text.secondary">
        {SECURITY_LABEL[network.security]}
      </Typography>
      {!network.active && (
        <Button size="small" onClick={onConnect} disabled={busy}>
          Connect
        </Button>
      )}
      <Button size="small" color="error" onClick={onForget} disabled={busy}>
        Forget
      </Button>
    </Stack>
  );
}

/**
 * Choosing a wireless network from the browser (issue #194).
 *
 * The companion app is the primary path — it is the transport that still works
 * when the device has no network at all — so this leads with "save for later",
 * which is what the issue asks the web for: setting up known networks ahead of
 * time. Joining now is offered, behind a confirmation, because a parent
 * standing in front of the device with a laptop should not have to reach for a
 * phone.
 */
export function WifiPanel() {
  const queryClient = useQueryClient();
  const [selected, setSelected] = useState<WifiNetwork | null>(null);
  const [manualOpen, setManualOpen] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const scan = useQuery<WifiScanView>({
    queryKey: ["wifi_networks"],
    queryFn: getWifiNetworks,
    refetchInterval: (query) =>
      query.state.data?.join.state === "connecting"
        ? JOINING_REFRESH_MS
        : REFRESH_MS,
  });
  const saved = useQuery<SavedWifiNetwork[]>({
    queryKey: ["wifi_saved_networks"],
    queryFn: getSavedWifiNetworks,
    // A device with no adapter answers this with an error; there is nothing
    // useful to retry and the panel says so from `scan` instead.
    enabled: scan.data?.supported ?? false,
    refetchInterval: REFRESH_MS,
  });

  const refresh = () => {
    void queryClient.invalidateQueries({ queryKey: ["wifi_networks"] });
    void queryClient.invalidateQueries({ queryKey: ["wifi_saved_networks"] });
  };

  /** Turn a failed RPC into the sentence the daemon sent. */
  const report = (e: unknown, fallback: string) => {
    const message =
      (e as { response?: { data?: { message?: string } } })?.response?.data
        ?.message ?? fallback;
    setError(message);
  };

  const rescan = useMutation({
    mutationFn: scanWifi,
    onSuccess: () => setNotice("Scanning…"),
    onError: (e) => report(e, "Could not start a scan."),
  });

  const save = useMutation({
    mutationFn: saveWifiNetwork,
    onSuccess: (_result, request) => {
      setSelected(null);
      setManualOpen(false);
      setError(null);
      // A join says nothing of its own: the daemon's join state already
      // shows "connecting" and then how it ended. A notice here outlived it,
      // reading "Connecting to X…" under "Connected to X." until dismissed.
      setNotice(
        request.connect
          ? null
          : `Saved ${request.ssid}. This device can join it on its own.`,
      );
      refresh();
    },
    onError: (e) => report(e, "Could not save the network."),
  });

  const connect = useMutation({
    mutationFn: connectWifiNetwork,
    onSuccess: () => {
      setError(null);
      // As for a join from the form: the join state reports this one.
      setNotice(null);
      refresh();
    },
    onError: (e) => report(e, "Could not join the network."),
  });

  const forget = useMutation({
    mutationFn: forgetWifiNetwork,
    onSuccess: (removed) => {
      setError(null);
      setNotice(
        removed ? "Network forgotten." : "That network was already gone.",
      );
      refresh();
    },
    onError: (e) => report(e, "Could not forget the network."),
  });

  const busy = save.isPending || connect.isPending || forget.isPending;

  if (scan.isLoading) {
    return (
      <Card>
        <CardContent>
          <Stack direction="row" spacing={1} sx={{ alignItems: "center" }}>
            <CircularProgress size={16} />
            <Typography variant="body2">Reading Wi-Fi…</Typography>
          </Stack>
        </CardContent>
      </Card>
    );
  }

  // A device with no radio is not a device with no networks in range, and
  // saying the second would send somebody walking around the house.
  if (!scan.data?.supported) {
    return (
      <Card>
        <CardContent>
          <Typography variant="subtitle1" gutterBottom>
            Wi-Fi
          </Typography>
          <Typography variant="body2" color="text.secondary">
            This device has no Wi-Fi adapter that NetworkManager is managing.
          </Typography>
        </CardContent>
      </Card>
    );
  }

  const view = scan.data;
  const failure = joinFailureText(view.join);

  return (
    <Card>
      <CardContent>
        <Stack
          direction="row"
          spacing={1}
          sx={{ alignItems: "center", mb: 1, flexWrap: "wrap", rowGap: 0.5 }}
        >
          <Typography variant="subtitle1" sx={{ flexGrow: 1 }}>
            Wi-Fi
          </Typography>
          <Button size="small" onClick={() => setManualOpen(true)}>
            Add network
          </Button>
          <Tooltip title="Scan again">
            <IconButton
              size="small"
              aria-label="Scan again"
              onClick={() => rescan.mutate()}
              disabled={rescan.isPending}
            >
              <RefreshIcon fontSize="inherit" />
            </IconButton>
          </Tooltip>
        </Stack>

        <Stack spacing={1.5}>
          {!view.radio_enabled && (
            <Alert severity="warning">
              The Wi-Fi radio is switched off on this device. Turn it back on at
              the device — this page cannot.
            </Alert>
          )}

          {!view.can_configure && (
            <Alert severity="warning">
              <AlertTitle>This device cannot save a network</AlertTitle>
              It can list what is in range and join networks it already knows,
              but not remember a new one. The Health page says what is missing;
              until it is fixed, use administrator mode on the device itself.
            </Alert>
          )}

          {view.join.state === "connecting" && (
            <Alert severity="info" icon={<CircularProgress size={16} />}>
              Connecting to {view.join.ssid}… this can take up to a minute.
            </Alert>
          )}
          {view.join.state === "connected" && (
            <Alert severity="success">Connected to {view.join.ssid}.</Alert>
          )}
          {failure && <Alert severity="error">{failure}</Alert>}
          {error && (
            <Alert severity="error" onClose={() => setError(null)}>
              {error}
            </Alert>
          )}
          {notice && !failure && (
            <Alert severity="info" onClose={() => setNotice(null)}>
              {notice}
            </Alert>
          )}

          <Box>
            <Typography variant="subtitle2" color="text.secondary" gutterBottom>
              In range
            </Typography>
            {view.networks.length === 0 ? (
              <Typography variant="body2" color="text.secondary">
                No networks found yet.
              </Typography>
            ) : (
              <Stack spacing={0.5}>
                {view.networks.map((network) => {
                  const canJoin = joinable(network.security);
                  return (
                    <Stack
                      key={`${network.ssid}:${network.security}`}
                      direction="row"
                      spacing={1}
                      sx={{
                        alignItems: "center",
                        flexWrap: "wrap",
                        rowGap: 0.5,
                      }}
                    >
                      <SignalIcon percent={network.signal_percent} />
                      {network.security === "open" ? (
                        <Tooltip title="Open network">
                          <LockOpenIcon fontSize="small" color="action" />
                        </Tooltip>
                      ) : (
                        <Tooltip title={SECURITY_LABEL[network.security]}>
                          <LockIcon fontSize="small" color="action" />
                        </Tooltip>
                      )}
                      <Typography variant="body2" sx={{ flexGrow: 1 }}>
                        {network.ssid}
                      </Typography>
                      {network.active && (
                        <Chip size="small" color="success" label="Connected" />
                      )}
                      {network.saved && !network.active && (
                        <Chip size="small" variant="outlined" label="Saved" />
                      )}
                      {canJoin ? (
                        !network.active && (
                          <Button
                            size="small"
                            disabled={busy || !view.can_configure}
                            onClick={() => setSelected(network)}
                          >
                            {network.saved ? "Reconnect" : "Join"}
                          </Button>
                        )
                      ) : (
                        // Shown rather than hidden: a network missing from the
                        // list reads as a device that cannot see it.
                        <Tooltip
                          title={
                            network.security === "enterprise"
                              ? "Enterprise networks need certificates; use administrator mode."
                              : "WEP is not supported."
                          }
                        >
                          <Chip
                            size="small"
                            variant="outlined"
                            label="Not supported"
                          />
                        </Tooltip>
                      )}
                    </Stack>
                  );
                })}
              </Stack>
            )}
            {view.truncated && (
              <Typography variant="caption" color="text.secondary">
                More networks are in range than can be listed.
              </Typography>
            )}
          </Box>

          {(saved.data?.length ?? 0) > 0 && (
            <>
              <Divider />
              <Box>
                <Typography
                  variant="subtitle2"
                  color="text.secondary"
                  gutterBottom
                >
                  Saved networks
                </Typography>
                <Stack spacing={0.5}>
                  {saved.data?.map((network) => (
                    <SavedRow
                      key={network.id}
                      network={network}
                      busy={busy}
                      onConnect={() => connect.mutate(network.id)}
                      onForget={() => forget.mutate(network.id)}
                    />
                  ))}
                </Stack>
              </Box>
            </>
          )}
        </Stack>
      </CardContent>

      <JoinDialog
        // Keyed on the network, so the dialog is a fresh component per
        // network. Without it a password typed for one network, then
        // cancelled, is still in the box when a different one is picked.
        key={selected ? `${selected.ssid}:${selected.security}` : "none"}
        network={selected}
        open={selected !== null}
        busy={save.isPending}
        onClose={() => setSelected(null)}
        onSubmit={(password, shouldConnect) =>
          selected &&
          save.mutate({
            ssid: selected.ssid,
            security: selected.security,
            password,
            hidden: false,
            connect: shouldConnect,
          })
        }
      />
      <ManualDialog
        open={manualOpen}
        busy={save.isPending}
        onClose={() => setManualOpen(false)}
        onSubmit={(ssid, security, password, hidden) =>
          save.mutate({ ssid, security, password, hidden, connect: false })
        }
      />
    </Card>
  );
}
