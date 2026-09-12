/**
 * What is going up, and what it is waiting for (issue #195).
 *
 * Mounted above the pages rather than inside the Files tab, so a parent who
 * starts a 2 GB video and goes to look at today's usage comes back to a
 * progress bar instead of to nothing. It renders nothing at all when there is
 * nothing in flight.
 */
import Box from "@mui/material/Box";
import Button from "@mui/material/Button";
import IconButton from "@mui/material/IconButton";
import LinearProgress from "@mui/material/LinearProgress";
import Paper from "@mui/material/Paper";
import Stack from "@mui/material/Stack";
import Tooltip from "@mui/material/Tooltip";
import Typography from "@mui/material/Typography";
import CloseIcon from "@mui/icons-material/Close";
import ErrorOutlineIcon from "@mui/icons-material/ErrorOutlined";
import { formatBytes } from "./format";
import { useUploads, type Transfer } from "./useUploads";

export function TransferTray() {
  const { transfers, cancel, replace, dismiss, clearFinished } = useUploads();
  if (transfers.length === 0) return null;

  const active = transfers.filter(
    (t) => t.status === "queued" || t.status === "sending",
  ).length;

  return (
    <Paper
      elevation={8}
      sx={{
        position: "fixed",
        right: { xs: 8, sm: 16 },
        bottom: { xs: 72, sm: 16 },
        width: { xs: "calc(100% - 16px)", sm: 380 },
        maxHeight: 320,
        overflow: "auto",
        zIndex: (theme) => theme.zIndex.snackbar,
      }}
    >
      <Box
        sx={{
          px: 2,
          py: 1,
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          position: "sticky",
          top: 0,
          bgcolor: "background.paper",
        }}
      >
        <Typography variant="subtitle2">
          {active > 0 ? `Sending ${active} file${active === 1 ? "" : "s"}` : "Transfers"}
        </Typography>
        <Button size="small" onClick={clearFinished}>
          Clear
        </Button>
      </Box>
      <Stack divider={<Box sx={{ borderTop: 1, borderColor: "divider" }} />}>
        {transfers.map((transfer) => (
          <TransferRow
            key={transfer.id}
            transfer={transfer}
            onCancel={() => cancel(transfer.id)}
            onReplace={() => replace(transfer.id)}
            onDismiss={() => dismiss(transfer.id)}
          />
        ))}
      </Stack>
    </Paper>
  );
}

function TransferRow({
  transfer,
  onCancel,
  onReplace,
  onDismiss,
}: {
  transfer: Transfer;
  onCancel: () => void;
  onReplace: () => void;
  onDismiss: () => void;
}) {
  const pending = transfer.status === "queued" || transfer.status === "sending";
  const percent =
    transfer.total > 0 ? Math.round((transfer.sent / transfer.total) * 100) : 0;

  return (
    <Box sx={{ px: 2, py: 1 }}>
      <Box sx={{ display: "flex", alignItems: "center", gap: 1 }}>
        <Box sx={{ minWidth: 0, flex: 1 }}>
          <Typography variant="body2" noWrap title={transfer.name}>
            {transfer.name}
          </Typography>
          <Typography variant="caption" color="text.secondary" noWrap>
            {statusLine(transfer)}
          </Typography>
        </Box>
        {transfer.status === "conflict" ? (
          <Button size="small" onClick={onReplace}>
            Replace
          </Button>
        ) : null}
        {transfer.status === "error" ? (
          <Tooltip title={transfer.error ?? ""}>
            <ErrorOutlineIcon color="error" fontSize="small" />
          </Tooltip>
        ) : null}
        <IconButton
          size="small"
          aria-label={pending ? `Cancel ${transfer.name}` : `Dismiss ${transfer.name}`}
          onClick={pending ? onCancel : onDismiss}
        >
          <CloseIcon fontSize="small" />
        </IconButton>
      </Box>
      {transfer.status === "sending" ? (
        <LinearProgress
          variant={transfer.total > 0 ? "determinate" : "indeterminate"}
          value={percent}
          sx={{ mt: 0.5 }}
        />
      ) : null}
    </Box>
  );
}

function statusLine(transfer: Transfer): string {
  switch (transfer.status) {
    case "queued":
      return "Waiting";
    case "sending":
      return `${formatBytes(transfer.sent)} of ${formatBytes(transfer.total)}`;
    case "conflict":
      return "Something is already there";
    case "done":
      return `Sent · ${formatBytes(transfer.total)}`;
    case "cancelled":
      return "Cancelled";
    case "error":
      return transfer.error ?? "Failed";
  }
}
