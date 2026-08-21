/**
 * What the daemon's own validator says about the document.
 *
 * The three report kinds are shown differently on purpose: a syntax error has a
 * position and blocks everything downstream, a version mismatch means this
 * build should not be editing the file at all, and semantic errors are a list
 * to work through.
 */
import Alert from "@mui/material/Alert";
import AlertTitle from "@mui/material/AlertTitle";
import Box from "@mui/material/Box";
import Chip from "@mui/material/Chip";
import Stack from "@mui/material/Stack";
import Typography from "@mui/material/Typography";
import CheckCircleIcon from "@mui/icons-material/CheckCircle";
import type { Issue, Report } from "../model/report";

export function IssueList({
  report,
  onSelectEntry,
  onSelectGroup,
  compact,
}: {
  report: Report | null;
  onSelectEntry?: (id: string) => void;
  onSelectGroup?: (id: string) => void;
  compact?: boolean;
}) {
  if (!report) {
    return (
      <Typography variant="body2" color="text.secondary">
        Checking…
      </Typography>
    );
  }

  if (report.kind === "syntax") {
    return (
      <Alert severity="error">
        <AlertTitle>This is not valid TOML</AlertTitle>
        Line {report.line}, column {report.column}: {report.message}
      </Alert>
    );
  }

  if (report.kind === "version") {
    return (
      <Alert severity="error">
        <AlertTitle>Unsupported config version</AlertTitle>
        This file says <code>config_version = {report.found}</code>, and this editor
        understands version {report.expected}. Editing it here could produce a file the
        daemon rejects.
      </Alert>
    );
  }

  if (report.errors.length === 0) {
    return (
      <Stack direction="row" spacing={1} sx={{ alignItems: "center" }}>
        <CheckCircleIcon color="success" fontSize="small" />
        <Typography variant="body2" color="success.main">
          Valid — the daemon would accept this.
        </Typography>
      </Stack>
    );
  }

  return (
    <Stack spacing={1}>
      {!compact && (
        <Typography variant="body2" sx={{ fontWeight: 600 }}>
          {report.errors.length} problem{report.errors.length === 1 ? "" : "s"}
        </Typography>
      )}
      {report.errors.map((issue, i) => (
        <IssueRow
          key={i}
          issue={issue}
          onSelectEntry={onSelectEntry}
          onSelectGroup={onSelectGroup}
        />
      ))}
    </Stack>
  );
}

function IssueRow({
  issue,
  onSelectEntry,
  onSelectGroup,
}: {
  issue: Issue;
  onSelectEntry?: (id: string) => void;
  onSelectGroup?: (id: string) => void;
}) {
  const target = issue.entry_id
    ? { label: issue.entry_id, go: () => onSelectEntry?.(issue.entry_id as string) }
    : issue.group_id
      ? { label: issue.group_id, go: () => onSelectGroup?.(issue.group_id as string) }
      : null;

  return (
    <Alert
      severity="error"
      sx={{ py: 0.25 }}
      action={
        target ? (
          <Chip size="small" label={target.label} onClick={target.go} clickable />
        ) : undefined
      }
    >
      <Box>
        <Typography variant="body2">{issue.message}</Typography>
        {issue.value && !target && (
          <Typography variant="caption" color="text.secondary">
            offending value: <code>{issue.value}</code>
          </Typography>
        )}
      </Box>
    </Alert>
  );
}
