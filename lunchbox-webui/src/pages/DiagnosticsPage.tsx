import Alert from "@mui/material/Alert";
import AlertTitle from "@mui/material/AlertTitle";
import Box from "@mui/material/Box";
import Card from "@mui/material/Card";
import CardContent from "@mui/material/CardContent";
import Chip from "@mui/material/Chip";
import Stack from "@mui/material/Stack";
import Typography from "@mui/material/Typography";
import CheckCircleOutlineIcon from "@mui/icons-material/CheckCircleOutlineOutlined";
import { useQuery } from "@tanstack/react-query";
import { listDiagnostics, listEntries } from "../api/client";
import type { Diagnostic, DiagnosticSeverity } from "../api/types";
import { diagnosticEntryId } from "../api/types";
import { Spinner } from "../components/Spinner";

const SEVERITY_LABEL: Record<DiagnosticSeverity, string> = {
  critical: "Critical",
  warning: "Warning",
  info: "Info",
};

/**
 * MUI severities, which drive both the colour and the icon. `critical` maps to
 * "error" rather than "warning": the config promises a protection the device
 * is not providing, which is a different thing from a feature being degraded.
 */
const SEVERITY_KIND: Record<DiagnosticSeverity, "error" | "warning" | "info"> = {
  critical: "error",
  warning: "warning",
  info: "info",
};

function since(d: Diagnostic): string {
  const started = new Date(d.since);
  if (Number.isNaN(started.getTime())) return "";
  return `since ${started.toLocaleString()}`;
}

/**
 * One condition. The title says what is wrong, the body says what to do about
 * it, and the chip names the activity when the problem belongs to one — a
 * parent should not have to work out which of their child's activities a
 * message is about.
 */
function DiagnosticCard({
  diagnostic,
  entryLabel,
}: {
  diagnostic: Diagnostic;
  entryLabel: string | null;
}) {
  return (
    <Alert severity={SEVERITY_KIND[diagnostic.severity]} variant="outlined">
      <AlertTitle sx={{ mb: 0.5 }}>{diagnostic.message}</AlertTitle>
      {diagnostic.remedy && (
        <Typography variant="body2" sx={{ mb: 1 }}>
          {diagnostic.remedy}
        </Typography>
      )}
      <Stack
        direction="row"
        spacing={1}
        sx={{ alignItems: "center", flexWrap: "wrap", rowGap: 0.5 }}
      >
        <Chip
          size="small"
          label={SEVERITY_LABEL[diagnostic.severity]}
          color={SEVERITY_KIND[diagnostic.severity]}
        />
        {entryLabel && <Chip size="small" variant="outlined" label={entryLabel} />}
        <Typography variant="caption" color="text.secondary">
          {since(diagnostic)}
        </Typography>
      </Stack>
    </Alert>
  );
}

/**
 * Everything currently wrong with the device, for an administrator.
 *
 * Ordering comes from the daemon (most severe first) rather than being redone
 * here, so this page and the companion app agree on what is most important.
 */
export function DiagnosticsPage() {
  const { data, isLoading, error } = useQuery({
    queryKey: ["diagnostics"],
    queryFn: listDiagnostics,
  });

  // Only to turn an entry id into the label a parent recognises. A failure
  // here must not hide the diagnostics themselves, so it is not awaited on.
  const { data: entries } = useQuery({
    queryKey: ["entries"],
    queryFn: () => listEntries(),
  });

  const labelFor = (id: string | null): string | null => {
    if (!id) return null;
    return entries?.find((e) => e.entry_id === id)?.label ?? id;
  };

  if (isLoading) return <Spinner />;

  if (error) {
    return (
      <Alert severity="error">
        <AlertTitle>Could not load diagnostics</AlertTitle>
        {String(error)}
      </Alert>
    );
  }

  const items = data?.items ?? [];

  return (
    <Stack spacing={2}>
      <Box>
        <Typography variant="h5">Device health</Typography>
        <Typography variant="body2" color="text.secondary">
          Problems with this device that need someone to fix them. These are
          separate from a child running out of time — they mean the device is
          not doing something its configuration says it should.
        </Typography>
      </Box>

      {data?.truncated && (
        <Alert severity="warning">
          There are more problems than can be listed here. Fix these and reload
          to see the rest.
        </Alert>
      )}

      {items.length === 0 ? (
        <Card>
          <CardContent>
            <Stack direction="row" spacing={1.5} sx={{ alignItems: "center" }}>
              <CheckCircleOutlineIcon color="success" />
              <Box>
                <Typography variant="subtitle1">Nothing to fix</Typography>
                <Typography variant="body2" color="text.secondary">
                  Every dependency is installed and every configured protection
                  is in effect.
                </Typography>
              </Box>
            </Stack>
          </CardContent>
        </Card>
      ) : (
        <Stack spacing={1.5}>
          {items.map((d) => (
            <DiagnosticCard
              key={`${d.code}:${diagnosticEntryId(d) ?? "service"}`}
              diagnostic={d}
              entryLabel={labelFor(diagnosticEntryId(d))}
            />
          ))}
        </Stack>
      )}
    </Stack>
  );
}
