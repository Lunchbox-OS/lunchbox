/**
 * Everything about one activity, in tabs.
 *
 * The split is by how often things change rather than by where they sit in the
 * schema: what launches and when it is allowed are edited constantly, the rest
 * is set once. Tab labels carry a dot when that tab holds a validation error,
 * so a problem in a collapsed section is still visible.
 */
import { useState } from "react";
import Alert from "@mui/material/Alert";
import Badge from "@mui/material/Badge";
import Box from "@mui/material/Box";
import FormControlLabel from "@mui/material/FormControlLabel";
import MenuItem from "@mui/material/MenuItem";
import Stack from "@mui/material/Stack";
import Switch from "@mui/material/Switch";
import Tab from "@mui/material/Tab";
import Tabs from "@mui/material/Tabs";
import TextField from "@mui/material/TextField";
import Typography from "@mui/material/Typography";
import { useConfigDoc } from "../doc/ConfigDocProvider";
import { useFields } from "../doc/useFields";
import { entryPath, insert, set, unset } from "../doc/patches";
import type { RawConfig, RawEntry, RawEntryKind } from "../model/config.generated";
import { issuesForEntry } from "../model/report";
import { BrowserEditor, FirewallEditor, InternetEditor } from "./NetworkEditors";
import { BrightnessEditor, VolumeEditor } from "./RestrictionEditors";
import { InputCompatEditor, RequiresInputEditor } from "./InputEditors";
import { IssueList } from "./IssueList";
import { KindEditor } from "./KindEditor";
import { LimitsEditor } from "./LimitsEditor";
import { ScheduleEditor } from "./ScheduleEditor";
import { Section } from "./Section";
import { TokensEditor } from "./TokensEditor";
import { WarningTimeline } from "./WarningTimeline";

type TabKey = "basics" | "schedule" | "limits" | "advanced";

export function EntryDetail({ entry, config }: { entry: RawEntry; config: RawConfig }) {
  const { apply, report, endGesture } = useConfigDoc();
  const [tab, setTab] = useState<TabKey>("basics");
  const base = entryPath(entry.id);
  const f = useFields(base);

  const issues = issuesForEntry(report, entry.id);
  const group = config.groups?.find((g) => g.id === entry.group);

  const warnings = entry.warnings ?? [];
  const warningsPath = `${base}.warnings`;

  const effectiveMaxRun =
    entry.limits?.max_run_seconds ??
    group?.limits?.max_run_seconds ??
    config.service?.default_max_run_seconds ??
    null;

  return (
    <Stack spacing={2}>
      {issues.length > 0 && <IssueList report={report} compact />}

      <Tabs value={tab} onChange={(_, v) => setTab(v as TabKey)} variant="scrollable">
        <Tab value="basics" label="Basics" />
        <Tab value="schedule" label="Schedule" />
        <Tab value="limits" label="Limits" />
        <Tab
          value="advanced"
          label={
            <Badge color="primary" variant="dot" invisible={!hasAdvanced(entry)}>
              <span>Advanced</span>
            </Badge>
          }
        />
      </Tabs>

      {tab === "basics" && (
        <Stack spacing={2}>
          <Stack direction="row" spacing={2}>
            <TextField
              size="small"
              label="Label"
              value={entry.label}
              onChange={(e) => f.setField("label", e.target.value)}
              sx={{ flex: 1 }}
              helperText="What the child sees on the tile."
            />
            <TextField
              size="small"
              label="Icon (optional)"
              value={entry.icon ?? ""}
              onChange={(e) => f.setField("icon", e.target.value)}
              helperText="Theme name or absolute path; autodetected when empty."
            />
          </Stack>

          <TextField
            size="small"
            select
            label="Category"
            value={entry.group ?? ""}
            onChange={(e) => f.setField("group", e.target.value || undefined)}
            sx={{ maxWidth: 320 }}
            helperText="Shares a schedule and a combined daily quota with its members."
          >
            <MenuItem value="">
              <em>None</em>
            </MenuItem>
            {(config.groups ?? []).map((g) => (
              <MenuItem key={g.id} value={g.id}>
                {g.label}
              </MenuItem>
            ))}
          </TextField>

          <Box>
            <Typography variant="subtitle2" sx={{ mb: 1 }}>
              What it launches
            </Typography>
            <KindEditor
              kind={entry.kind}
              onChange={(kind: RawEntryKind) => apply(set(`${base}.kind`, kind as never))}
            />
          </Box>

          <Section
            title="Disabled"
            description="Keeps the activity in the file but hides it entirely."
            present={entry.disabled === true}
            onTogglePresent={(on) =>
              on ? f.setField("disabled", true) : f.unsetField("disabled")
            }
          >
            <TextField
              size="small"
              fullWidth
              label="Reason (shown to the caregiver)"
              value={entry.disabled_reason ?? ""}
              onChange={(e) => f.setField("disabled_reason", e.target.value)}
            />
          </Section>
        </Stack>
      )}

      {tab === "schedule" && (
        <Stack spacing={2}>
          {group && (
            <Alert severity="info">
              <strong>{group.label}</strong> has its own schedule. Both must allow a moment
              for this activity to appear, so the effective availability is the overlap —
              outlined on the grid.
            </Alert>
          )}
          <ScheduleEditor subject={{ kind: "entry", id: entry.id }} availability={entry.availability} />
        </Stack>
      )}

      {tab === "limits" && (
        <Stack spacing={4}>
          <LimitsEditor
            subject={{ kind: "entry", id: entry.id }}
            limits={entry.limits}
            serviceMaxRun={config.service?.default_max_run_seconds}
            serviceCooldownGrace={config.service?.cooldown_min_session_seconds}
            groupLimits={group?.limits}
            groupLabel={group?.label}
          />

          <Section
            title="Warnings"
            description="Overrides [service.default_warnings] for this activity."
            present={entry.warnings != null}
            onTogglePresent={(on) =>
              on
                ? apply(set(warningsPath, [{ seconds_before: 300, severity: "warn" }] as never))
                : apply(unset(warningsPath))
            }
          >
            <WarningTimeline
              warnings={warnings}
              maxRunSeconds={effectiveMaxRun}
              onChange={(i, next) => {
                for (const [key, value] of Object.entries(next)) {
                  const path = `${warningsPath}[${i}].${key}`;
                  if (value === null || value === undefined) apply(unset(path));
                  else apply(set(path, value as never), `drag:${path}`);
                }
              }}
              onCommit={endGesture}
              onAdd={(w) => apply(insert(warningsPath, w as never))}
              onRemove={(i) => apply(unset(`${warningsPath}[${i}]`))}
            />
          </Section>

          <TokensEditor
            basePath={base}
            entryId={entry.id}
            tokens={entry.tokens}
            config={config}
          />
        </Stack>
      )}

      {tab === "advanced" && (
        <Stack spacing={1}>
          <InternetEditor path={`${base}.internet`} value={entry.internet} />
          <FirewallEditor path={`${base}.firewall`} value={entry.firewall} />
          <BrowserEditor path={`${base}.browser`} value={entry.browser} />
          <InputCompatEditor
            basePath={base}
            compat={entry.input_compat ?? []}
            options={entry.input_compat_options}
          />
          <RequiresInputEditor basePath={base} devices={entry.requires_input ?? []} />
          <VolumeEditor path={`${base}.volume`} value={entry.volume} />
          <BrightnessEditor path={`${base}.brightness`} value={entry.brightness} />

          <Section title="Behaviour" defaultExpanded>
            <Stack>
              <FormControlLabel
                control={
                  <Switch
                    checked={entry.confirm_on_close ?? true}
                    onChange={(e) => f.setField("confirm_on_close", e.target.checked)}
                  />
                }
                label="Confirm before the HUD's X ends this activity"
              />
              <Typography variant="caption" color="text.secondary" sx={{ ml: 6, mt: -1 }}>
                Worth turning off only for activities that lose nothing when closed
                instantly.
              </Typography>
              <FormControlLabel
                sx={{ mt: 1 }}
                control={
                  <Switch
                    checked={entry.xwayland_native_resolution ?? false}
                    onChange={(e) =>
                      e.target.checked
                        ? f.setField("xwayland_native_resolution", true)
                        : f.unsetField("xwayland_native_resolution")
                    }
                  />
                }
                label="Drop the compositor scale to 1.0 while running"
              />
              <Typography variant="caption" color="text.secondary" sx={{ ml: 6, mt: -1 }}>
                For XWayland games that would otherwise render into a corner of the panel.
              </Typography>
            </Stack>
          </Section>
        </Stack>
      )}
    </Stack>
  );
}

/** Whether anything on the Advanced tab is actually set. */
function hasAdvanced(entry: RawEntry): boolean {
  return Boolean(
    entry.internet ||
      entry.firewall ||
      entry.browser ||
      entry.volume ||
      entry.brightness ||
      (entry.input_compat?.length ?? 0) > 0 ||
      (entry.requires_input?.length ?? 0) > 0 ||
      entry.xwayland_native_resolution ||
      entry.confirm_on_close === false,
  );
}
