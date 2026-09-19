import { useState } from "react";
import Alert from "@mui/material/Alert";
import AlertTitle from "@mui/material/AlertTitle";
import Box from "@mui/material/Box";
import Button from "@mui/material/Button";
import Card from "@mui/material/Card";
import CardContent from "@mui/material/CardContent";
import Chip from "@mui/material/Chip";
import Snackbar from "@mui/material/Snackbar";
import Stack from "@mui/material/Stack";
import Typography from "@mui/material/Typography";
import CloseIcon from "@mui/icons-material/Close";
import OpenInFullIcon from "@mui/icons-material/OpenInFull";
import RefreshIcon from "@mui/icons-material/Refresh";
import VisibilityIcon from "@mui/icons-material/Visibility";
import VisibilityOffIcon from "@mui/icons-material/VisibilityOff";
import WarningAmberIcon from "@mui/icons-material/WarningAmber";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  closeWindow,
  focusWindow,
  getServiceState,
  hideWindow,
  listWindows,
  showWindow,
} from "../api/client";
import type { WindowInfo, WindowOwner } from "../api/types";
import { Spinner } from "../components/Spinner";

function windowTitle(w: WindowInfo): string {
  return w.name || w.app_id || w.window_class || `Window ${w.id}`;
}

function windowSubtitle(w: WindowInfo): string {
  const parts: string[] = [];
  if (w.app_id) parts.push(`app_id=${w.app_id}`);
  else if (w.window_class) parts.push(`class=${w.window_class}`);
  if (w.pid !== null) parts.push(`pid=${w.pid}`);
  parts.push(`id=${w.id}`);
  return parts.join(" · ");
}

/**
 * A window nobody is supervising: an activity that survived its own teardown,
 * or a surface belonging to no session at all. These are the ones this page
 * exists for — lunchboxd reports them but deliberately will not close an
 * unrecognized one by itself, so a caregiver has to make that call.
 */
function isOrphan(w: WindowInfo, adminMode: boolean): boolean {
  // In administrator mode a caregiver is deliberately opening things, so every
  // window is unowned by construction and none of them is a problem. Calling
  // them unsupervised would put a red banner over the caregiver's own work —
  // the exact false positive the owner attribution exists to remove.
  if (adminMode) return false;
  return w.owner === "escaped" || w.owner === "unowned";
}

const OWNER_LABEL: Record<WindowOwner, string> = {
  lunchbox: "Lunchbox",
  activity: "Activity",
  escaped: "Escaped",
  unowned: "Unowned",
};

const OWNER_HELP: Record<WindowOwner, string> = {
  lunchbox: "Part of lunchbox itself — the launcher, the HUD, or a background helper.",
  activity: "Belongs to the session running right now.",
  escaped:
    "This activity outlived its own teardown. Its session is over and lunchbox is still trying to kill it.",
  unowned:
    "No process lunchbox knows about. Either it was started outside lunchbox, or an activity got away without lunchbox noticing.",
};

function OwnerChip({ owner }: { owner: WindowOwner }) {
  const label = OWNER_LABEL[owner];
  if (owner === "escaped") {
    return <Chip size="small" label={label} color="error" title={OWNER_HELP[owner]} />;
  }
  if (owner === "unowned") {
    return <Chip size="small" label={label} color="warning" title={OWNER_HELP[owner]} />;
  }
  return (
    <Chip
      size="small"
      label={label}
      variant="outlined"
      color="default"
      title={OWNER_HELP[owner]}
    />
  );
}

interface WindowCardProps {
  w: WindowInfo;
  adminMode: boolean;
  busy: boolean;
  onClose: (id: number) => void;
  onHide: (id: number) => void;
  onShow: (id: number) => void;
  onFocus: (id: number) => void;
}

function WindowCard({ w, adminMode, busy, onClose, onHide, onShow, onFocus }: WindowCardProps) {
  const orphan = isOrphan(w, adminMode);
  return (
    <Card
      variant="outlined"
      sx={
        orphan
          ? {
              borderColor: w.owner === "escaped" ? "error.main" : "warning.main",
            }
          : undefined
      }
    >
      <CardContent sx={{ "&:last-child": { pb: 2 } }}>
        <Box sx={{ display: "flex", justifyContent: "space-between", alignItems: "flex-start", gap: 1 }}>
          <Box sx={{ minWidth: 0, flex: 1 }}>
            <Typography variant="subtitle1" sx={{ fontWeight: 600, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
              {windowTitle(w)}
            </Typography>
            <Typography variant="caption" color="text.secondary" sx={{ overflowWrap: "anywhere" }}>
              {windowSubtitle(w)}
            </Typography>
          </Box>
          <Stack direction="row" spacing={0.5} sx={{ flexWrap: "wrap", justifyContent: "flex-end", rowGap: 0.5 }}>
            <OwnerChip owner={w.owner} />
            {w.focused && <Chip size="small" label="Focused" color="primary" />}
            {w.in_scratchpad ? (
              <Chip size="small" label="Scratchpad" color="warning" variant="outlined" />
            ) : w.visible ? (
              <Chip size="small" label="On screen" color="success" variant="outlined" />
            ) : (
              <Chip size="small" label="Hidden" variant="outlined" />
            )}
          </Stack>
        </Box>
        {w.workspace && !w.in_scratchpad && (
          <Typography variant="caption" color="text.disabled" sx={{ display: "block", mt: 0.5 }}>
            Workspace: {w.workspace}
          </Typography>
        )}
        {orphan && (
          <Typography variant="caption" color="text.secondary" sx={{ display: "block", mt: 0.5 }}>
            {OWNER_HELP[w.owner]}
          </Typography>
        )}
        <Stack direction="row" spacing={1} sx={{ mt: 1.5, flexWrap: "wrap", rowGap: 1 }}>
          {/*
            Focus is offered only for a window that is on screen and not
            already focused. A scratchpad row keeps Show as its one action —
            sway's `focus` would raise it as well, so this is about not
            offering two buttons that do the same thing — and focusing the
            focused window is a no-op that still costs a round trip.
          */}
          {!w.in_scratchpad && !w.focused && (
            <Button
              size="small"
              variant="outlined"
              startIcon={<OpenInFullIcon />}
              onClick={() => onFocus(w.id)}
              disabled={busy}
            >
              Focus
            </Button>
          )}
          {w.in_scratchpad ? (
            <Button
              size="small"
              variant="outlined"
              startIcon={<VisibilityIcon />}
              onClick={() => onShow(w.id)}
              disabled={busy}
            >
              Show
            </Button>
          ) : (
            <Button
              size="small"
              variant="outlined"
              startIcon={<VisibilityOffIcon />}
              onClick={() => onHide(w.id)}
              disabled={busy}
            >
              Hide
            </Button>
          )}
          <Button
            size="small"
            // The whole point of the orphan section is that closing is the
            // action being asked for, so it leads there rather than sitting
            // among equals.
            variant={orphan ? "contained" : "outlined"}
            color="error"
            startIcon={<CloseIcon />}
            onClick={() => onClose(w.id)}
            disabled={busy}
          >
            Close
          </Button>
        </Stack>
      </CardContent>
    </Card>
  );
}

export function WindowsPage() {
  const queryClient = useQueryClient();
  const { data, isPending, isFetching, error, refetch } = useQuery({
    queryKey: ["debug-windows"],
    queryFn: listWindows,
    refetchInterval: 5000,
  });

  const [msg, setMsg] = useState<{ text: string; ok: boolean } | null>(null);
  const flash = (text: string, ok = true) => {
    setMsg({ text, ok });
    setTimeout(() => setMsg(null), 3000);
  };

  const invalidate = () =>
    queryClient.invalidateQueries({ queryKey: ["debug-windows"] });

  const closeMutation = useMutation({
    mutationFn: closeWindow,
    onSuccess: () => {
      flash("Close requested");
      invalidate();
    },
    onError: (e) => flash(String(e), false),
  });
  const hideMutation = useMutation({
    mutationFn: hideWindow,
    onSuccess: () => {
      flash("Moved to scratchpad");
      invalidate();
    },
    onError: (e) => flash(String(e), false),
  });
  const showMutation = useMutation({
    mutationFn: showWindow,
    onSuccess: () => {
      flash("Pulled from scratchpad");
      invalidate();
    },
    onError: (e) => flash(String(e), false),
  });
  const focusMutation = useMutation({
    mutationFn: focusWindow,
    onSuccess: () => {
      flash("Focused");
      invalidate();
    },
    onError: (e) => flash(String(e), false),
  });

  const busy =
    closeMutation.isPending ||
    hideMutation.isPending ||
    showMutation.isPending ||
    focusMutation.isPending;

  // Only for the orphan framing: a window panel that polled the whole snapshot
  // for its own sake would be spending an RPC on state this page never draws.
  const { data: state } = useQuery({
    queryKey: ["service-state"],
    queryFn: getServiceState,
    refetchInterval: 5000,
  });
  const adminMode = state?.admin_mode ?? false;

  const windows = data ?? [];
  // Orphans on the scratchpad stay under the scratchpad heading: they are
  // stashed rather than loose on the child's screen, which is the same line
  // lunchboxd's own reconciliation sweep draws before it warns.
  const orphaned = windows.filter((w) => !w.in_scratchpad && isOrphan(w, adminMode));
  const onScreen = windows.filter((w) => !w.in_scratchpad && !isOrphan(w, adminMode));
  const scratchpad = windows.filter((w) => w.in_scratchpad);

  const handlers = {
    onClose: (id: number) => closeMutation.mutate(id),
    onHide: (id: number) => hideMutation.mutate(id),
    onShow: (id: number) => showMutation.mutate(id),
    onFocus: (id: number) => focusMutation.mutate(id),
  };

  return (
    <Box sx={{ display: "flex", flexDirection: "column", gap: 2 }}>
      <Box sx={{ display: "flex", justifyContent: "space-between", alignItems: "center" }}>
        <Typography variant="h6" sx={{ fontWeight: 700 }}>Sway Windows</Typography>
        <Button
          size="small"
          variant="outlined"
          startIcon={isFetching ? <Spinner size={16} /> : <RefreshIcon />}
          onClick={() => refetch()}
          disabled={isFetching}
        >
          Refresh
        </Button>
      </Box>

      <Typography variant="body2" color="text.secondary">
        Windows the compositor is tracking. Auto-refreshes every 5s.
      </Typography>

      <Snackbar open={!!msg} autoHideDuration={3000} onClose={() => setMsg(null)}>
        <Alert severity={msg?.ok ? "success" : "error"} onClose={() => setMsg(null)} sx={{ width: "100%" }}>
          {msg?.text}
        </Alert>
      </Snackbar>

      {error && (
        <Alert severity="error">{error instanceof Error ? error.message : String(error)}</Alert>
      )}

      {isPending && !data && (
        <Box sx={{ display: "flex", justifyContent: "center", py: 6 }}>
          <Spinner />
        </Box>
      )}

      {data && windows.length === 0 && (
        <Typography color="text.secondary" sx={{ textAlign: "center", py: 4 }}>
          No windows reported
        </Typography>
      )}

      {orphaned.length > 0 && (
        <Box>
          <Alert severity="warning" icon={<WarningAmberIcon />} sx={{ mb: 1 }}>
            <AlertTitle>
              {orphaned.length === 1
                ? "1 window on screen has no session behind it"
                : `${orphaned.length} windows on screen have no session behind them`}
            </AlertTitle>
            Time spent in these is not metered and no time limit will end them.
            Lunchbox reports them but will not close a window it does not
            recognize on its own — that call is yours.
          </Alert>
          <Typography variant="subtitle2" color="text.secondary" sx={{ mb: 1 }}>
            Unsupervised ({orphaned.length})
          </Typography>
          <Stack spacing={1}>
            {orphaned.map((w) => (
              <WindowCard key={w.id} w={w} adminMode={adminMode} busy={busy} {...handlers} />
            ))}
          </Stack>
        </Box>
      )}

      {onScreen.length > 0 && (
        <Box>
          <Typography variant="subtitle2" color="text.secondary" sx={{ mb: 1 }}>
            On Screen ({onScreen.length})
          </Typography>
          <Stack spacing={1}>
            {onScreen.map((w) => (
              <WindowCard key={w.id} w={w} adminMode={adminMode} busy={busy} {...handlers} />
            ))}
          </Stack>
        </Box>
      )}

      {scratchpad.length > 0 && (
        <Box>
          <Typography variant="subtitle2" color="text.secondary" sx={{ mb: 1 }}>
            Scratchpad ({scratchpad.length})
          </Typography>
          <Stack spacing={1}>
            {scratchpad.map((w) => (
              <WindowCard key={w.id} w={w} adminMode={adminMode} busy={busy} {...handlers} />
            ))}
          </Stack>
        </Box>
      )}
    </Box>
  );
}
