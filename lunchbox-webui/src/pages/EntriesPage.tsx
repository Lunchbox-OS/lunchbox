import { useState } from "react";
import Alert from "@mui/material/Alert";
import Box from "@mui/material/Box";
import Button from "@mui/material/Button";
import Card from "@mui/material/Card";
import CardContent from "@mui/material/CardContent";
import Chip from "@mui/material/Chip";
import InputAdornment from "@mui/material/InputAdornment";
import Snackbar from "@mui/material/Snackbar";
import Stack from "@mui/material/Stack";
import TextField from "@mui/material/TextField";
import Typography from "@mui/material/Typography";
import SearchIcon from "@mui/icons-material/Search";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import LinearProgress from "@mui/material/LinearProgress";
import {
  deleteOverride,
  launchSession,
  listDiagnostics,
  listEntries,
  listGroups,
  listOverrides,
  upsertOverride,
  adjustTokens,
} from "../api/client";
import {
  diagnosticEntryId,
  durationToSecs,
  formatDurationHuman,
  reasonLabel,
  type DailyOverride,
  type Diagnostic,
  type EntryView,
  type GroupView,
  type TokenStatus,
} from "../api/types";

import { useEvents } from "../hooks/useEvents";
import { Spinner } from "../components/Spinner";

/**
 * Anything a daily override can target (issue #5): an activity, or a whole
 * category. `subject` is the wire key — a bare entry id, or `group:<id>`.
 */
type LimitTarget = { subject: string; label: string };

const entryTarget = (e: EntryView): LimitTarget => ({
  subject: e.entry_id,
  label: e.label,
});
const groupTarget = (g: GroupView): LimitTarget => ({
  subject: `group:${g.group_id}`,
  label: g.label,
});

function todayString(): string {
  // Use the local date — overrides are stored under the local-time day, so
  // a UTC ISO slice rolls forward by a day in the evening (and produces a
  // mismatch where the engine looks under the local date and finds nothing).
  const d = new Date();
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}

export function EntriesPage() {
  const queryClient = useQueryClient();
  const today = todayString();

  const { data: entries, isPending: entriesLoading } = useQuery({
    queryKey: ["entries"],
    queryFn: () => listEntries(),
  });
  const { data: groups } = useQuery({
    queryKey: ["groups"],
    queryFn: () => listGroups(),
  });
  const { data: overrides } = useQuery({
    queryKey: ["overrides", today],
    queryFn: () => listOverrides(today),
  });

  const [busyId, setBusyId] = useState<string | null>(null);
  const [msg, setMsg] = useState<{ text: string; ok: boolean } | null>(null);
  const [search, setSearch] = useState("");

  useEvents();

  const flash = (text: string, ok = true) => {
    setMsg({ text, ok });
    setTimeout(() => setMsg(null), 3000);
  };

  const invalidateAll = () => {
    queryClient.invalidateQueries({ queryKey: ["entries"] });
    queryClient.invalidateQueries({ queryKey: ["groups"] });
    queryClient.invalidateQueries({ queryKey: ["overrides"] });
  };

  const launchMutation = useMutation({
    mutationFn: (entry_id: string) => launchSession(entry_id),
    onMutate: (entry_id) => setBusyId(entry_id),
    onSuccess: (res) => {
      if (res.result === "approved") {
        flash("Session started!");
        invalidateAll();
      } else {
        const reason = res.reasons[0] ? reasonLabel(res.reasons[0]) : "Denied";
        flash(`Cannot launch: ${reason}`, false);
      }
    },
    onError: (e) => flash(String(e), false),
    onSettled: () => setBusyId(null),
  });

  // These four take a LimitTarget rather than an EntryView so the category
  // cards below drive the same code path as the activity cards — the only
  // difference is the subject they address.
  const disableMutation = useMutation({
    mutationFn: (t: LimitTarget) => upsertOverride(t.subject, false, null, today),
    onMutate: (t) => setBusyId(t.subject),
    onSuccess: (_, t) => {
      flash(`${t.label}: disabled for today`);
      invalidateAll();
    },
    onError: (e) => flash(String(e), false),
    onSettled: () => setBusyId(null),
  });

  const clearMutation = useMutation({
    mutationFn: (t: LimitTarget) => deleteOverride(t.subject, today),
    onMutate: (t) => setBusyId(t.subject),
    onSuccess: (_, t) => {
      flash(`${t.label}: override cleared`);
      invalidateAll();
    },
    onError: (e) => flash(String(e), false),
    onSettled: () => setBusyId(null),
  });

  const enableMutation = useMutation({
    mutationFn: (t: LimitTarget) => upsertOverride(t.subject, true, null, today),
    onMutate: (t) => setBusyId(t.subject),
    onSuccess: (_, t) => {
      flash(`${t.label}: enabled for today`);
      invalidateAll();
    },
    onError: (e) => flash(String(e), false),
    onSettled: () => setBusyId(null),
  });

  // Granting banked time (issue #8). Unlike the quota delta this is not an
  // override field: it moves the same balance that playing a source activity
  // fills and the gated activity spends, so it commits straight through.
  const adjustTokensMutation = useMutation({
    mutationFn: ({ target, deltaSeconds }: { target: LimitTarget; deltaSeconds: number }) =>
      adjustTokens(target.subject, deltaSeconds),
    onMutate: ({ target }) => setBusyId(target.subject),
    onSuccess: (status, { target, deltaSeconds }) => {
      const sign = deltaSeconds > 0 ? "+" : "−";
      flash(
        `${target.label}: earned time ${sign}${formatDurationHuman(Math.abs(deltaSeconds))} ` +
          `(now ${formatDurationHuman(status.balance.secs)})`,
      );
      invalidateAll();
    },
    onError: (e: Error) => flash(e.message),
    onSettled: () => setBusyId(null),
  });

  // Instant-commit quota adjustment — matches how availability changes
  // are already applied. Preserves the current availability override
  // (if any) so bumping quota doesn't accidentally clear an "Off today"
  // / "On today ★" state.
  const adjustQuotaMutation = useMutation({
    mutationFn: ({
      target,
      newDeltaSeconds,
      currentAvailability,
    }: {
      target: LimitTarget;
      newDeltaSeconds: number;
      currentAvailability: boolean | null;
    }) =>
      upsertOverride(
        target.subject,
        currentAvailability,
        newDeltaSeconds === 0 ? null : newDeltaSeconds,
        today,
      ),
    onMutate: ({ target }) => setBusyId(target.subject),
    onSuccess: (_, { target: entry, newDeltaSeconds }) => {
      const sign = newDeltaSeconds > 0 ? "+" : newDeltaSeconds < 0 ? "−" : "";
      const abs = formatDurationHuman(Math.abs(newDeltaSeconds));
      flash(
        newDeltaSeconds === 0
          ? `${entry.label}: quota adjustment cleared`
          : `${entry.label}: quota ${sign}${abs}`,
      );
      invalidateAll();
    },
    onError: (e) => flash(String(e), false),
    onSettled: () => setBusyId(null),
  });

  const loading = entriesLoading && !entries;
  const filtered = (entries ?? []).filter((e) =>
    e.label.toLowerCase().includes(search.toLowerCase()),
  );
  // Keyed by limit subject; an entry's subject is its bare ID, so the lookup
  // below still works, and group overrides simply don't match an entry.
  const overrideMap = new Map((overrides ?? []).map((ov) => [ov.subject, ov]));

  // Per-activity problems (issue #143), indexed so each card can show its own.
  // Fetched once for the page rather than per card.
  const { data: diagnosticSet } = useQuery({
    queryKey: ["diagnostics"],
    queryFn: listDiagnostics,
  });
  const diagnosticsByEntry = new Map<string, Diagnostic[]>();
  for (const d of diagnosticSet?.items ?? []) {
    const id = diagnosticEntryId(d);
    if (!id) continue;
    diagnosticsByEntry.set(id, [...(diagnosticsByEntry.get(id) ?? []), d]);
  }

  const groupLabels = new Map((groups ?? []).map((g) => [g.group_id, g.label]));

  return (
    <Box sx={{ display: "flex", flexDirection: "column", gap: 2 }}>

      <Snackbar open={!!msg} autoHideDuration={3000} onClose={() => setMsg(null)}>
        <Alert severity={msg?.ok ? "success" : "error"} onClose={() => setMsg(null)} sx={{ width: "100%" }}>
          {msg?.text}
        </Alert>
      </Snackbar>

      <TextField
        size="small"
        placeholder="Search…"
        value={search}
        onChange={(e) => setSearch(e.target.value)}
        slotProps={{
          input: {
            startAdornment: (
              <InputAdornment position="start">
                <SearchIcon fontSize="small" />
              </InputAdornment>
            ),
          },
        }}
      />

      {loading && (
        <Box sx={{ display: "flex", justifyContent: "center", py: 6 }}>
          <Spinner />
        </Box>
      )}

      {(groups ?? []).length > 0 && (
        <>
          <Typography variant="h6" sx={{ fontWeight: 700 }}>Categories</Typography>
          <Typography variant="caption" color="text.secondary" sx={{ mt: -1.5 }}>
            These limits are shared by every activity in the category.
          </Typography>
          <Stack spacing={1.5}>
            {(groups ?? []).map((group) => {
              const target = groupTarget(group);
              const override = overrideMap.get(target.subject);
              return (
                <GroupCard
                  key={group.group_id}
                  group={group}
                  override={override}
                  busy={busyId === target.subject}
                  onDisableToday={() => disableMutation.mutate(target)}
                  onClear={() => clearMutation.mutate(target)}
                  onEnable={() => enableMutation.mutate(target)}
                  onAdjustQuota={(stepSeconds) => {
                    const current = override?.quota_delta_seconds ?? 0;
                    adjustQuotaMutation.mutate({
                      target,
                      newDeltaSeconds: current + stepSeconds,
                      currentAvailability: override?.availability ?? null,
                    });
                  }}
                  onAdjustTokens={(deltaSeconds) =>
                    adjustTokensMutation.mutate({ target, deltaSeconds })
                  }
                />
              );
            })}
          </Stack>
        </>
      )}

      <Typography variant="h6" sx={{ fontWeight: 700 }}>Activities</Typography>

      <Stack spacing={1.5}>
        {filtered.map((entry) => {
          const override = overrideMap.get(entry.entry_id);
          return (
            <EntryCard
              key={entry.entry_id}
              entry={entry}
              diagnostics={diagnosticsByEntry.get(entry.entry_id) ?? []}
              groupLabel={entry.group ? groupLabels.get(entry.group) : undefined}
              override={override}
              busy={busyId === entry.entry_id}
              onLaunch={() => launchMutation.mutate(entry.entry_id)}
              onDisableToday={() => disableMutation.mutate(entryTarget(entry))}
              onClear={() => clearMutation.mutate(entryTarget(entry))}
              onEnable={() => enableMutation.mutate(entryTarget(entry))}
              onAdjustQuota={(stepSeconds) => {
                const current = override?.quota_delta_seconds ?? 0;
                adjustQuotaMutation.mutate({
                  target: entryTarget(entry),
                  newDeltaSeconds: current + stepSeconds,
                  currentAvailability: override?.availability ?? null,
                });
              }}
              onAdjustTokens={(deltaSeconds) =>
                adjustTokensMutation.mutate({ target: entryTarget(entry), deltaSeconds })
              }
            />
          );
        })}
        {!loading && filtered.length === 0 && (
          <Typography color="text.secondary" sx={{ textAlign: "center", py: 4 }}>
            No activities found
          </Typography>
        )}
      </Stack>
    </Box>
  );
}

/** Step (seconds) for the ± quota buttons — matches the Android
 *  companion's `−5` / `+5` stepper. */
const QUOTA_STEP_SECS = 5 * 60;

/** Step (seconds) for granting banked time. Same size as the quota step. */
const TOKEN_STEP_SECS = 5 * 60;

/**
 * Banked time on a token gate, with a stepper to grant or revoke it (issue #8).
 *
 * Shown for entries and categories alike; the caller supplies the subject via
 * `onAdjust`. Says how much is banked and what it takes to unlock, because
 * granting blind is how a caregiver ends up handing out a balance that is
 * still below `minimum_seconds` and wondering why nothing changed.
 */
function TokenRow({
  tokens,
  busy,
  onAdjust,
}: {
  tokens: TokenStatus;
  busy: boolean;
  onAdjust: (deltaSeconds: number) => void;
}) {
  const balance = tokens.balance.secs;
  const minimum = tokens.minimum.secs;
  const atCeiling =
    tokens.max_balance != null && balance >= tokens.max_balance.secs;

  return (
    <Box sx={{ display: "flex", alignItems: "center", gap: 1, mt: 0.5, flexWrap: "wrap" }}>
      <Typography variant="caption" color="text.secondary">
        Earned {formatDurationHuman(balance)}
        {!tokens.unlocked && minimum > 0
          ? ` — needs ${formatDurationHuman(minimum)} to unlock`
          : ""}
        {atCeiling ? " (at the maximum)" : ""}
      </Typography>
      <Button
        size="small"
        variant="text"
        color="inherit"
        onClick={() => onAdjust(-TOKEN_STEP_SECS)}
        disabled={busy || balance === 0}
        sx={{ minWidth: 0, px: 1 }}
      >
        −5m
      </Button>
      <Button
        size="small"
        variant="text"
        color="inherit"
        onClick={() => onAdjust(TOKEN_STEP_SECS)}
        disabled={busy || atCeiling}
        sx={{ minWidth: 0, px: 1 }}
      >
        +5m
      </Button>
    </Box>
  );
}

function EntryCard({
  entry,
  diagnostics,
  groupLabel,
  override,
  busy,
  onLaunch,
  onDisableToday,
  onClear,
  onEnable,
  onAdjustQuota,
  onAdjustTokens,
}: {
  entry: EntryView;
  /** Administrator-facing problems with this activity (issue #143). */
  diagnostics: Diagnostic[];
  groupLabel: string | undefined;
  override: DailyOverride | undefined;
  busy: boolean;
  onLaunch: () => void;
  onDisableToday: () => void;
  onClear: () => void;
  onEnable: () => void;
  onAdjustQuota: (stepSeconds: number) => void;
  onAdjustTokens: (deltaSeconds: number) => void;
}) {
  const manuallyDisabled = override?.availability === false;
  const manuallyEnabled = override?.availability === true;
  const maxRunSecs = durationToSecs(entry.max_run_if_started_now);
  const quotaDelta = override?.quota_delta_seconds ?? 0;

  return (
    <Card
      variant="outlined"
      sx={{ opacity: !entry.enabled && !manuallyEnabled ? 0.75 : 1 }}
    >
      <CardContent sx={{ pb: "12px !important" }}>
        <Box sx={{ display: "flex", justifyContent: "space-between", alignItems: "flex-start", mb: 0.5 }}>
          <Box>
            <Typography sx={{ fontWeight: 600 }}>{entry.label}</Typography>
            <Typography variant="caption" color="text.secondary">
              {entry.kind_tag}
            </Typography>
            {/* Signals that this activity's schedule and budget are shared,
                so a caregiver isn't puzzled when it goes away because a
                sibling was played (issue #5). */}
            {groupLabel && (
              <Chip
                label={groupLabel}
                size="small"
                variant="outlined"
                sx={{ ml: 1, height: 18, fontSize: "0.65rem" }}
              />
            )}
          </Box>
          {manuallyDisabled ? (
            <Chip label="Off today" size="small" color="default" />
          ) : manuallyEnabled ? (
            <Chip label="On today ★" size="small" color="primary" />
          ) : entry.enabled ? (
            <Chip label="Available" size="small" color="success" />
          ) : (
            <Chip label="Unavailable" size="small" color="default" />
          )}
        </Box>

        {/* Every reason, not just the first: an activity blocked by both a
            cooldown and a spent quota would otherwise reveal the second only
            once the first is cleared. Matches the category card below. */}
        {!entry.enabled && entry.reasons.length > 0 && (
          <Typography variant="caption" color="text.secondary">
            {entry.reasons.map(reasonLabel).join(" · ")}
          </Typography>
        )}

        {/* A misconfiguration on this activity (issue #143), shown next to the
            availability reasons rather than only on the Health page: the
            reason line says the activity is unavailable, this says why in a
            way somebody can act on. */}
        {diagnostics.map((d) => (
          <Typography
            key={d.code}
            variant="caption"
            component="div"
            sx={{
              color: d.severity === "critical" ? "error.main" : "warning.main",
            }}
          >
            {d.message}
            {d.remedy ? ` — ${d.remedy}` : ""}
          </Typography>
        ))}

        {entry.enabled && maxRunSecs > 0 && (
          <Typography variant="caption" color="text.secondary">
            Up to {formatDurationHuman(maxRunSecs)}
          </Typography>
        )}

        {entry.tokens && (
          <TokenRow tokens={entry.tokens} busy={busy} onAdjust={onAdjustTokens} />
        )}

        {/* Quota adjustment: the Android companion has this same
            ±5 min stepper on the entry detail screen. On the web we
            place it inline on the card so parents can grant or claw
            back time from the same view they use for
            Enable/Disable Today. */}
        <Box sx={{ display: "flex", alignItems: "center", gap: 1, mt: 0.5 }}>
          <Typography variant="caption" color="text.secondary">
            Quota {quotaDelta === 0
              ? "unchanged"
              : `${quotaDelta > 0 ? "+" : "−"}${formatDurationHuman(Math.abs(quotaDelta))}`}
          </Typography>
          <Button
            size="small"
            variant="text"
            color="inherit"
            onClick={() => onAdjustQuota(-QUOTA_STEP_SECS)}
            disabled={busy}
            sx={{ minWidth: 0, px: 1 }}
          >
            −5m
          </Button>
          <Button
            size="small"
            variant="text"
            color="inherit"
            onClick={() => onAdjustQuota(QUOTA_STEP_SECS)}
            disabled={busy}
            sx={{ minWidth: 0, px: 1 }}
          >
            +5m
          </Button>
        </Box>

        <Box sx={{ display: "flex", gap: 1, mt: 1.5, flexWrap: "wrap" }}>
          {(manuallyDisabled || manuallyEnabled || quotaDelta !== 0) ? (
            <Button size="small" variant="outlined" onClick={onClear} disabled={busy}>
              {busy ? <Spinner size={14} /> : "Clear Override"}
            </Button>
          ) : (
            <>
              <Button size="small" variant="text" color="inherit" onClick={onDisableToday} disabled={busy}>
                {busy ? <Spinner size={14} /> : "Disable Today"}
              </Button>
              {!entry.enabled && (
                <Button size="small" variant="outlined" color="primary" onClick={onEnable} disabled={busy}>
                  {busy ? <Spinner size={14} /> : "Enable Today"}
                </Button>
              )}
            </>
          )}
          {entry.enabled && (
            <Button size="small" variant="contained" onClick={onLaunch} disabled={busy}>
              {busy ? <Spinner size={14} /> : "Launch"}
            </Button>
          )}
        </Box>
      </CardContent>
    </Card>
  );
}

/**
 * A category card: combined usage against the shared budget, whatever is
 * currently restricting the category, and the same override controls as an
 * activity — pointed at the `group:<id>` subject.
 */
function GroupCard({
  group,
  override,
  busy,
  onDisableToday,
  onClear,
  onEnable,
  onAdjustQuota,
  onAdjustTokens,
}: {
  group: GroupView;
  override: DailyOverride | undefined;
  busy: boolean;
  onDisableToday: () => void;
  onClear: () => void;
  onEnable: () => void;
  onAdjustQuota: (stepSeconds: number) => void;
  onAdjustTokens: (deltaSeconds: number) => void;
}) {
  const manuallyDisabled = override?.availability === false;
  const manuallyEnabled = override?.availability === true;
  const quotaDelta = override?.quota_delta_seconds ?? 0;
  const usedSecs = durationToSecs(group.used_today);
  const quotaSecs = durationToSecs(group.daily_quota);
  const capSecs = durationToSecs(group.max_run_if_started_now);

  return (
    <Card variant="outlined" sx={{ opacity: !group.enabled && !manuallyEnabled ? 0.75 : 1 }}>
      <CardContent sx={{ pb: "12px !important" }}>
        <Box sx={{ display: "flex", justifyContent: "space-between", alignItems: "flex-start", mb: 0.5 }}>
          <Box>
            <Typography sx={{ fontWeight: 600 }}>{group.label}</Typography>
            <Typography variant="caption" color="text.secondary">
              {group.member_ids.length} activit{group.member_ids.length === 1 ? "y" : "ies"}
            </Typography>
          </Box>
          {manuallyDisabled ? (
            <Chip label="Off today" size="small" color="default" />
          ) : manuallyEnabled ? (
            <Chip label="On today ★" size="small" color="primary" />
          ) : group.enabled ? (
            <Chip label="Available" size="small" color="success" />
          ) : (
            <Chip label="Unavailable" size="small" color="default" />
          )}
        </Box>

        {!group.enabled && group.reasons.length > 0 && (
          <Typography variant="caption" color="text.secondary">
            {group.reasons.map(reasonLabel).join(" · ")}
          </Typography>
        )}

        {/* Combined usage — the whole point of a category, so show it even
            when the budget is unlimited. */}
        <Box sx={{ mt: 0.5 }}>
          <Typography variant="caption" color="text.secondary">
            {quotaSecs > 0
              ? `${formatDurationHuman(usedSecs)} of ${formatDurationHuman(quotaSecs)} used today`
              : `${formatDurationHuman(usedSecs)} used today (no daily limit)`}
          </Typography>
          {quotaSecs > 0 && (
            <LinearProgress
              variant="determinate"
              value={Math.min(100, (usedSecs / quotaSecs) * 100)}
              sx={{ mt: 0.5, height: 6, borderRadius: 3 }}
            />
          )}
        </Box>

        {group.enabled && capSecs > 0 && (
          <Typography variant="caption" color="text.secondary" sx={{ display: "block", mt: 0.5 }}>
            Up to {formatDurationHuman(capSecs)} per session
          </Typography>
        )}

        {group.tokens && (
          <TokenRow tokens={group.tokens} busy={busy} onAdjust={onAdjustTokens} />
        )}

        <Box sx={{ display: "flex", alignItems: "center", gap: 1, mt: 0.5 }}>
          <Typography variant="caption" color="text.secondary">
            Quota {quotaDelta === 0
              ? "unchanged"
              : `${quotaDelta > 0 ? "+" : "−"}${formatDurationHuman(Math.abs(quotaDelta))}`}
          </Typography>
          <Button size="small" variant="text" color="inherit" onClick={() => onAdjustQuota(-QUOTA_STEP_SECS)} disabled={busy} sx={{ minWidth: 0, px: 1 }}>
            −5m
          </Button>
          <Button size="small" variant="text" color="inherit" onClick={() => onAdjustQuota(QUOTA_STEP_SECS)} disabled={busy} sx={{ minWidth: 0, px: 1 }}>
            +5m
          </Button>
        </Box>

        <Box sx={{ display: "flex", gap: 1, mt: 1.5, flexWrap: "wrap" }}>
          {(manuallyDisabled || manuallyEnabled || quotaDelta !== 0) ? (
            <Button size="small" variant="outlined" onClick={onClear} disabled={busy}>
              {busy ? <Spinner size={14} /> : "Clear Override"}
            </Button>
          ) : (
            <>
              <Button size="small" variant="outlined" onClick={onEnable} disabled={busy}>
                Enable Today
              </Button>
              <Button size="small" variant="outlined" color="warning" onClick={onDisableToday} disabled={busy}>
                Disable Today
              </Button>
            </>
          )}
        </Box>
      </CardContent>
    </Card>
  );
}
