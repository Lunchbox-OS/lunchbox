import { useState } from "react";
import Alert from "@mui/material/Alert";
import Box from "@mui/material/Box";
import Button from "@mui/material/Button";
import Card from "@mui/material/Card";
import CardContent from "@mui/material/CardContent";
import Chip from "@mui/material/Chip";
import Snackbar from "@mui/material/Snackbar";
import Stack from "@mui/material/Stack";
import Typography from "@mui/material/Typography";
import CloseIcon from "@mui/icons-material/Close";
import RefreshIcon from "@mui/icons-material/Refresh";
import VisibilityIcon from "@mui/icons-material/Visibility";
import VisibilityOffIcon from "@mui/icons-material/VisibilityOff";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  closeWindow,
  hideWindow,
  listWindows,
  showWindow,
} from "../api/client";
import type { WindowInfo } from "../api/types";
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

interface WindowCardProps {
  w: WindowInfo;
  busy: boolean;
  onClose: (id: number) => void;
  onHide: (id: number) => void;
  onShow: (id: number) => void;
}

function WindowCard({ w, busy, onClose, onHide, onShow }: WindowCardProps) {
  return (
    <Card variant="outlined">
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
        <Stack direction="row" spacing={1} sx={{ mt: 1.5, flexWrap: "wrap", rowGap: 1 }}>
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
            variant="outlined"
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

  const busy =
    closeMutation.isPending || hideMutation.isPending || showMutation.isPending;

  const windows = data?.windows ?? [];
  const onScreen = windows.filter((w) => !w.in_scratchpad);
  const scratchpad = windows.filter((w) => w.in_scratchpad);

  const handlers = {
    onClose: (id: number) => closeMutation.mutate(id),
    onHide: (id: number) => hideMutation.mutate(id),
    onShow: (id: number) => showMutation.mutate(id),
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

      {onScreen.length > 0 && (
        <Box>
          <Typography variant="subtitle2" color="text.secondary" sx={{ mb: 1 }}>
            On Screen ({onScreen.length})
          </Typography>
          <Stack spacing={1}>
            {onScreen.map((w) => (
              <WindowCard key={w.id} w={w} busy={busy} {...handlers} />
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
              <WindowCard key={w.id} w={w} busy={busy} {...handlers} />
            ))}
          </Stack>
        </Box>
      )}
    </Box>
  );
}
