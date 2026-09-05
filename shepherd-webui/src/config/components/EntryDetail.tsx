/**
 * One activity: everything a category also has, plus what only an activity has.
 *
 * The shared half — label, schedule, limits, token gate — lives in
 * [`SubjectDetail`]. What is left here is the part of `RawEntry` that
 * `RawGroup` has no equivalent for: what it launches, the hardware and network
 * it needs, and the per-activity overrides.
 */
import Box from "@mui/material/Box";
import FormControlLabel from "@mui/material/FormControlLabel";
import MenuItem from "@mui/material/MenuItem";
import Stack from "@mui/material/Stack";
import Switch from "@mui/material/Switch";
import TextField from "@mui/material/TextField";
import Typography from "@mui/material/Typography";
import Badge from "@mui/material/Badge";
import { useConfigDoc } from "../doc/ConfigDocProvider";
import { useFields } from "../doc/useFields";
import { entryPath, insert, set, unset } from "../doc/patches";
import type { RawConfig, RawEntry, RawEntryKind } from "../model/config.generated";
import { confirmsOnCloseByDefault, defaultInputCompat } from "../model/kindDefaults";
import {
  HUD_ORIENTATIONS,
  VERTICAL_HUD_DESCRIPTION,
} from "../model/hudOrientation";
import { BrowserEditor, FirewallEditor, InternetEditor } from "./NetworkEditors";
import { BrightnessEditor, VolumeEditor } from "./RestrictionEditors";
import { InputCompatEditor, RequiresInputEditor } from "./InputEditors";
import { KindEditor } from "./KindEditor";
import { Section } from "./Section";
import { SubjectDetail } from "./SubjectDetail";
import { WarningTimeline } from "./WarningTimeline";

export function EntryDetail({ entry, config }: { entry: RawEntry; config: RawConfig }) {
  const { apply, endGesture } = useConfigDoc();
  const base = entryPath(entry.id);
  const f = useFields(base);

  const group = config.groups?.find((g) => g.id === entry.group);
  const warnings = entry.warnings ?? [];
  const warningsPath = `${base}.warnings`;

  const effectiveMaxRun =
    entry.limits?.max_run_seconds ??
    group?.limits?.max_run_seconds ??
    config.service?.default_max_run_seconds ??
    null;

  return (
    <SubjectDetail
      subject={{ kind: "entry", id: entry.id }}
      config={config}
      basics={
        <>
          <TextField
            size="small"
            label="Icon (optional)"
            value={entry.icon ?? ""}
            onChange={(e) => f.setField("icon", e.target.value)}
            sx={{ maxWidth: 400 }}
            helperText="Theme name or absolute path; autodetected when empty."
          />

          <TextField
            size="small"
            select
            label="Category"
            value={entry.group ?? ""}
            onChange={(e) => f.setField("group", e.target.value || undefined)}
            sx={{ maxWidth: 400 }}
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
        </>
      }
      limitsExtra={
        // `RawGroup` has no `warnings`, so this is genuinely activity-only
        // rather than something a category is missing.
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
      }
      extraTabs={[
        {
          key: "advanced",
          label: (
            <Badge color="primary" variant="dot" invisible={!hasAdvanced(entry)}>
              <span>Advanced</span>
            </Badge>
          ),
          content: (
            <Stack spacing={1}>
              <InternetEditor path={`${base}.internet`} value={entry.internet} />
              <FirewallEditor path={`${base}.firewall`} value={entry.firewall} />
              <BrowserEditor path={`${base}.browser`} value={entry.browser} />
              <InputCompatEditor
                basePath={base}
                compat={entry.input_compat ?? defaultInputCompat(entry.kind)}
                explicit={entry.input_compat !== undefined && entry.input_compat !== null}
                kindDefault={defaultInputCompat(entry.kind)}
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
                        checked={entry.confirm_on_close ?? confirmsOnCloseByDefault(entry.kind)}
                        onChange={(e) => f.setField("confirm_on_close", e.target.checked)}
                      />
                    }
                    label="Confirm before the HUD's X ends this activity"
                  />
                  <Typography variant="caption" color="text.secondary" sx={{ ml: 6, mt: -1 }}>
                    Worth turning off only for activities that lose nothing when closed
                    instantly — which is why a book starts off.
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
                    For XWayland games that would otherwise render into a corner of the
                    panel.
                  </Typography>
                  <TextField
                    select
                    size="small"
                    sx={{ mt: 2 }}
                    label="HUD edge while this runs"
                    // Inheriting must read as inheriting rather than as a blank
                    // box — see the matching control on the Device page.
                    slotProps={{
                      select: { displayEmpty: true },
                      inputLabel: { shrink: true },
                    }}
                    value={entry.hud_orientation ?? ""}
                    onChange={(e) =>
                      e.target.value
                        ? f.setField("hud_orientation", e.target.value)
                        : f.unsetField("hud_orientation")
                    }
                  >
                    <MenuItem value="">Use the device setting</MenuItem>
                    {HUD_ORIENTATIONS.map((o) => (
                      <MenuItem key={o.value} value={o.value}>
                        {o.label}
                      </MenuItem>
                    ))}
                  </TextField>
                  <Typography variant="caption" color="text.secondary" sx={{ mt: -1 }}>
                    {VERTICAL_HUD_DESCRIPTION} Worth it for an activity whose own UI lives
                    along the top, or one that wants every horizontal pixel. The HUD moves
                    back to the device setting when the activity ends.
                  </Typography>
                </Stack>
              </Section>
            </Stack>
          ),
        },
      ]}
    />
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
      // An explicit empty list is a *choice* — a book refusing the gamepad
      // sidecar its kind would otherwise run — so it counts as set.
      (entry.input_compat?.length === 0 && defaultInputCompat(entry.kind).length > 0) ||
      (entry.requires_input?.length ?? 0) > 0 ||
      Boolean(entry.hud_orientation) ||
      entry.xwayland_native_resolution ||
      entry.confirm_on_close === false,
  );
}
