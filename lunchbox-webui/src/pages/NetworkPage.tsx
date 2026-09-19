import { useState } from "react";
import Alert from "@mui/material/Alert";
import AlertTitle from "@mui/material/AlertTitle";
import Box from "@mui/material/Box";
import Button from "@mui/material/Button";
import Card from "@mui/material/Card";
import CardContent from "@mui/material/CardContent";
import Chip from "@mui/material/Chip";
import Divider from "@mui/material/Divider";
import IconButton from "@mui/material/IconButton";
import Link from "@mui/material/Link";
import Snackbar from "@mui/material/Snackbar";
import Stack from "@mui/material/Stack";
import Tooltip from "@mui/material/Tooltip";
import Typography from "@mui/material/Typography";
import ContentCopyIcon from "@mui/icons-material/ContentCopyOutlined";
import LanIcon from "@mui/icons-material/LanOutlined";
import RouterIcon from "@mui/icons-material/RouterOutlined";
import SignalWifi4BarIcon from "@mui/icons-material/SignalWifi4Bar";
import { useQuery } from "@tanstack/react-query";
import { getNetworkStatus, getServiceState } from "../api/client";
import type {
  Connectivity,
  InternetStatusView,
  NetworkInterfaceView,
  NetworkStatusView,
  WebListenerView,
  WifiView,
} from "../api/types";
import { Spinner } from "../components/Spinner";

/**
 * Re-read while somebody is looking at the page. An address changes when a
 * cable is plugged in or a VPN comes up, which is exactly when this page is
 * open — but nothing pushes that, so the page asks.
 */
const REFRESH_MS = 10_000;

const CONNECTIVITY: Record<
  Connectivity,
  { label: string; color: "success" | "warning" | "error" | "default" }
> = {
  full: { label: "Online", color: "success" },
  // A captive portal is the one that looks connected and is not, so it says so
  // in its own words rather than as a shade of "limited".
  portal: { label: "Sign-in required", color: "warning" },
  limited: { label: "Limited", color: "warning" },
  none: { label: "Offline", color: "error" },
  unknown: { label: "Unknown", color: "default" },
};

/** Copy to the clipboard, reporting whether it worked. */
async function copy(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    // Insecure origins have no clipboard API, and this page is routinely served
    // over plain HTTP on a LAN. The address is on screen either way.
    return false;
  }
}

/** A value worth copying: shown in a monospace face with a copy button. */
function Copyable({
  value,
  onCopied,
}: {
  value: string;
  onCopied: (ok: boolean) => void;
}) {
  return (
    <Stack direction="row" spacing={0.5} sx={{ alignItems: "center" }}>
      <Typography variant="body2" sx={{ fontFamily: "monospace" }}>
        {value}
      </Typography>
      <Tooltip title="Copy">
        <IconButton
          size="small"
          aria-label={`Copy ${value}`}
          onClick={() => void copy(value).then(onCopied)}
        >
          <ContentCopyIcon fontSize="inherit" />
        </IconButton>
      </Tooltip>
    </Stack>
  );
}

function wifiSummary(wifi: WifiView): string {
  const parts: string[] = [];
  if (wifi.signal_percent != null) parts.push(`${wifi.signal_percent}% signal`);
  const mhz = wifi.frequency_mhz;
  if (mhz != null) {
    // The band is what a person debugging a weak signal actually reads.
    if (mhz >= 2400 && mhz <= 2500) parts.push("2.4 GHz");
    else if (mhz >= 4900 && mhz <= 5900) parts.push("5 GHz");
    else if (mhz >= 5925 && mhz <= 7125) parts.push("6 GHz");
    else parts.push(`${mhz} MHz`);
  }
  return parts.join(" · ");
}

/**
 * Where the web interface is listening — including when it is not.
 *
 * The "not" is the part that did not exist before: a management API that never
 * bound reached one line in a log, on the device whose web interface is how
 * somebody would have read it.
 */
function WebInterfaceCard({
  listener,
  urls,
  onCopied,
}: {
  listener: WebListenerView;
  urls: string[];
  onCopied: (ok: boolean) => void;
}) {
  if (listener.state === "disabled") {
    return (
      <Card>
        <CardContent>
          <Typography variant="subtitle1" gutterBottom>
            Web interface
          </Typography>
          <Typography variant="body2" color="text.secondary">
            Not enabled on this device. Set <code>service.management_api</code>{" "}
            in the configuration to serve this page over the network.
          </Typography>
        </CardContent>
      </Card>
    );
  }

  if (listener.state === "failed") {
    return (
      <Alert severity="warning" variant="outlined">
        <AlertTitle>The web interface is not serving</AlertTitle>
        <Typography variant="body2" sx={{ mb: 1 }}>
          It is configured for <code>{listener.addr}</code> and could not bind:{" "}
          {listener.error ?? "no reason given"}.
        </Typography>
        <Typography variant="body2" color="text.secondary">
          Check that address belongs to this device and that nothing else holds
          the port.
        </Typography>
      </Alert>
    );
  }

  if (listener.state === "binding") {
    return (
      <Alert severity="info" variant="outlined">
        <AlertTitle>The web interface is still starting</AlertTitle>
        <Typography variant="body2">
          Waiting for <code>{listener.addr}</code> to exist. An address on an
          interface that comes up late — a VPN, for instance — can take a while.
        </Typography>
      </Alert>
    );
  }

  return (
    <Card>
      <CardContent>
        <Stack
          direction="row"
          spacing={1}
          sx={{ alignItems: "center", mb: 1.5 }}
        >
          <RouterIcon color="action" />
          <Typography variant="subtitle1">Web interface</Typography>
          <Chip size="small" color="success" label="Serving" />
        </Stack>

        {urls.length === 0 ? (
          <Typography variant="body2" color="text.secondary">
            Listening on <code>{listener.addr}</code>. This device has no
            address another machine could reach it at.
          </Typography>
        ) : (
          <Stack spacing={1}>
            <Typography variant="body2" color="text.secondary">
              Open this page from another device at:
            </Typography>
            {urls.map((url) => (
              <Stack
                key={url}
                direction="row"
                spacing={0.5}
                sx={{ alignItems: "center" }}
              >
                <Link
                  href={url}
                  variant="body2"
                  sx={{ fontFamily: "monospace" }}
                >
                  {url}
                </Link>
                <Tooltip title="Copy">
                  <IconButton
                    size="small"
                    aria-label={`Copy ${url}`}
                    onClick={() => void copy(url).then(onCopied)}
                  >
                    <ContentCopyIcon fontSize="inherit" />
                  </IconButton>
                </Tooltip>
              </Stack>
            ))}
          </Stack>
        )}
      </CardContent>
    </Card>
  );
}

/** One interface: what it is, what it is called, and how to reach it. */
function InterfaceCard({
  iface,
  onCopied,
}: {
  iface: NetworkInterfaceView;
  onCopied: (ok: boolean) => void;
}) {
  return (
    <Card variant={iface.reachable ? "elevation" : "outlined"}>
      <CardContent>
        <Stack
          direction="row"
          spacing={1}
          sx={{ alignItems: "center", flexWrap: "wrap", rowGap: 0.5, mb: 1 }}
        >
          {iface.kind === "wifi" ? (
            <SignalWifi4BarIcon color="action" fontSize="small" />
          ) : (
            <LanIcon color="action" fontSize="small" />
          )}
          <Typography variant="subtitle1" sx={{ fontFamily: "monospace" }}>
            {iface.name}
          </Typography>
          <Chip size="small" variant="outlined" label={iface.kind} />
          {!iface.up && <Chip size="small" label="Down" />}
        </Stack>

        {iface.wifi &&
          (iface.wifi.ssid ? (
            <Typography variant="body2" sx={{ mb: 1 }}>
              Connected to <strong>{iface.wifi.ssid}</strong>
              {wifiSummary(iface.wifi) && ` — ${wifiSummary(iface.wifi)}`}
            </Typography>
          ) : (
            <Typography variant="body2" color="text.secondary" sx={{ mb: 1 }}>
              Not connected to a network.
            </Typography>
          ))}

        {iface.addresses.length === 0 ? (
          <Typography variant="body2" color="text.secondary">
            No address.
          </Typography>
        ) : (
          <Stack spacing={0.5}>
            {iface.addresses.map((a) => (
              <Stack
                key={`${a.address}/${a.prefix}`}
                direction="row"
                spacing={1}
                sx={{ alignItems: "center" }}
              >
                <Copyable value={a.address} onCopied={onCopied} />
                <Typography variant="caption" color="text.secondary">
                  /{a.prefix}
                </Typography>
              </Stack>
            ))}
          </Stack>
        )}

        {(iface.gateway || iface.dns.length > 0) && (
          <>
            <Divider sx={{ my: 1.5 }} />
            <Stack spacing={0.25}>
              {iface.gateway && (
                <Typography variant="caption" color="text.secondary">
                  Gateway {iface.gateway}
                </Typography>
              )}
              {iface.dns.length > 0 && (
                <Typography variant="caption" color="text.secondary">
                  DNS {iface.dns.join(", ")}
                </Typography>
              )}
            </Stack>
          </>
        )}
      </CardContent>
    </Card>
  );
}

/** The configured connectivity checks and their latest result. */
function ChecksCard({ checks }: { checks: InternetStatusView[] }) {
  if (checks.length === 0) return null;
  return (
    <Card>
      <CardContent>
        <Typography variant="subtitle1" gutterBottom>
          Connectivity checks
        </Typography>
        <Typography variant="body2" color="text.secondary" sx={{ mb: 1.5 }}>
          Activities that require the internet are held back when their check
          last failed.
        </Typography>
        <Stack spacing={1}>
          {checks.map((c) => (
            <Stack
              key={c.target}
              direction="row"
              spacing={1}
              sx={{ alignItems: "center", flexWrap: "wrap", rowGap: 0.5 }}
            >
              <Chip
                size="small"
                color={c.available ? "success" : "error"}
                label={c.available ? "Reachable" : "Unreachable"}
              />
              <Typography variant="body2" sx={{ fontFamily: "monospace" }}>
                {c.target}
              </Typography>
            </Stack>
          ))}
        </Stack>
      </CardContent>
    </Card>
  );
}

/**
 * Where this device is on the network (issue #182).
 *
 * Read-only: it answers "what is this thing's address and can I reach it",
 * which is the question somebody has when they want to SSH in or open this
 * page from somewhere else. Joining a wireless network is not here.
 */
export function NetworkPage() {
  const [copied, setCopied] = useState<string | null>(null);
  const [showAll, setShowAll] = useState(false);

  const { data, isLoading, error } = useQuery<NetworkStatusView>({
    queryKey: ["network"],
    queryFn: getNetworkStatus,
    refetchInterval: REFRESH_MS,
  });

  // The connectivity checks live on the service snapshot and nowhere else.
  // A failure here must not hide the addresses, so it is not awaited on.
  const { data: snapshot } = useQuery({
    queryKey: ["service-state"],
    queryFn: getServiceState,
    refetchInterval: REFRESH_MS,
  });

  const onCopied = (ok: boolean) =>
    setCopied(ok ? "Copied" : "Could not copy — select the text instead");

  if (isLoading) return <Spinner />;

  if (error || !data) {
    return (
      <Alert severity="error">
        <AlertTitle>Could not load the network status</AlertTitle>
        {String(error)}
      </Alert>
    );
  }

  const connectivity = CONNECTIVITY[data.connectivity];
  const reachable = data.interfaces.filter((i) => i.reachable);
  const rest = data.interfaces.filter((i) => !i.reachable);

  return (
    <Stack spacing={2}>
      <Box>
        <Stack
          direction="row"
          spacing={1}
          sx={{ alignItems: "center", flexWrap: "wrap", rowGap: 0.5 }}
        >
          <Typography variant="h5">Network</Typography>
          <Chip
            size="small"
            color={connectivity.color}
            label={connectivity.label}
          />
        </Stack>
        <Typography variant="body2" color="text.secondary">
          Where this device is on the network, and how to reach it from another
          machine.
        </Typography>
      </Box>

      {data.source === "unavailable" && (
        <Alert severity="warning">
          <AlertTitle>This device could not read its own network</AlertTitle>
          Neither NetworkManager nor the kernel's interface list answered. This
          is not the same as being offline.
        </Alert>
      )}

      {data.source === "interfaces" && (
        <Alert severity="info">
          NetworkManager is not running here, so the addresses below are real
          but the network name, gateway and DNS are not available.
        </Alert>
      )}

      <WebInterfaceCard
        listener={data.management_api}
        urls={data.management_urls ?? []}
        onCopied={onCopied}
      />

      <ChecksCard checks={snapshot?.internet_status ?? []} />

      <Box>
        <Typography variant="subtitle2" color="text.secondary" gutterBottom>
          {reachable.length === 0
            ? "No interface can be reached from another machine"
            : "Reachable from another machine"}
        </Typography>
        <Stack spacing={1.5}>
          {reachable.map((iface) => (
            <InterfaceCard
              key={iface.name}
              iface={iface}
              onCopied={onCopied}
            />
          ))}
        </Stack>
      </Box>

      {rest.length > 0 && (
        <Box>
          <Button size="small" onClick={() => setShowAll((v) => !v)}>
            {showAll
              ? "Hide other interfaces"
              : `Show ${rest.length} other interface${rest.length === 1 ? "" : "s"}`}
          </Button>
          {showAll && (
            <Stack spacing={1.5} sx={{ mt: 1.5 }}>
              {rest.map((iface) => (
                <InterfaceCard
                  key={iface.name}
                  iface={iface}
                  onCopied={onCopied}
                />
              ))}
            </Stack>
          )}
        </Box>
      )}

      {data.truncated && (
        <Alert severity="info">
          This device has more interfaces than can be listed here.
        </Alert>
      )}

      <Snackbar
        open={copied !== null}
        autoHideDuration={2000}
        onClose={() => setCopied(null)}
        message={copied}
      />
    </Stack>
  );
}
