import Alert from "@mui/material/Alert";
import Box from "@mui/material/Box";
import Button from "@mui/material/Button";
import Card from "@mui/material/Card";
import CardContent from "@mui/material/CardContent";
import Chip from "@mui/material/Chip";
import Stack from "@mui/material/Stack";
import Typography from "@mui/material/Typography";
import RefreshIcon from "@mui/icons-material/Refresh";
import { useQuery } from "@tanstack/react-query";
import { listWindows } from "../api/client";
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

function WindowCard({ w }: { w: WindowInfo }) {
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
      </CardContent>
    </Card>
  );
}

export function WindowsPage() {
  const { data, isPending, isFetching, error, refetch } = useQuery({
    queryKey: ["debug-windows"],
    queryFn: listWindows,
    refetchInterval: 5000,
  });

  const windows = data?.windows ?? [];
  const onScreen = windows.filter((w) => !w.in_scratchpad);
  const scratchpad = windows.filter((w) => w.in_scratchpad);

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
        Read-only view of windows the compositor is tracking. Auto-refreshes every 5s.
      </Typography>

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
              <WindowCard key={w.id} w={w} />
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
              <WindowCard key={w.id} w={w} />
            ))}
          </Stack>
        </Box>
      )}
    </Box>
  );
}
