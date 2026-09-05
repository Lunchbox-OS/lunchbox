/**
 * Session and daily budgets.
 *
 * Every value here cascades: `max_run_seconds` falls back to
 * `service.default_max_run_seconds`, and `cooldown_min_session_seconds`
 * cascades service -> group -> entry (`policy.rs`). What applies when a field
 * is left unset is drawn on the slider track rather than explained in prose.
 *
 * The daemon's own fallbacks come from `field-defaults.generated.ts` rather
 * than being restated here — they are resolved at policy load, so the JSON
 * Schema cannot carry them and they are generated from
 * `shepherd-config::LoadTimeDefaults` instead.
 */
import Stack from "@mui/material/Stack";
import Typography from "@mui/material/Typography";
import { useConfigDoc } from "../doc/ConfigDocProvider";
import { dragKey, set, subjectPath, unset, type Subject } from "../doc/patches";
import type { RawLimits } from "../model/config.generated";
import { LOAD_TIME_DEFAULTS } from "../model/field-defaults.generated";
import { DurationSlider, type InheritedValue } from "./DurationSlider";

interface Props {
  subject: Subject;
  limits: RawLimits | null | undefined;
  /** `service.default_max_run_seconds`, when set. */
  serviceMaxRun?: number | null;
  /** `service.cooldown_min_session_seconds`, when set. */
  serviceCooldownGrace?: number | null;
  /** `service.save_grace_seconds`, when set. */
  serviceSaveGrace?: number | null;
  /** The group's limits, for an entry that belongs to one. */
  groupLimits?: RawLimits | null;
  groupLabel?: string;
}

export function LimitsEditor({
  subject,
  limits,
  serviceMaxRun,
  serviceCooldownGrace,
  serviceSaveGrace,
  groupLimits,
  groupLabel,
}: Props) {
  const { apply, endGesture } = useConfigDoc();

  const field = (name: keyof RawLimits) => subjectPath(subject, "limits", name);

  const update = (name: keyof RawLimits) => (seconds: number | null) => {
    const path = field(name);
    if (seconds === null) apply(unset(path));
    else apply(set(path, seconds), dragKey(path));
  };

  const inheritedMaxRun: InheritedValue[] = [];
  if (groupLimits?.max_run_seconds)
    inheritedMaxRun.push({ label: `${groupLabel ?? "Category"} cap`, seconds: groupLimits.max_run_seconds });
  if (serviceMaxRun) inheritedMaxRun.push({ label: "Service default", seconds: serviceMaxRun });

  const inheritedQuota: InheritedValue[] = [];
  if (groupLimits?.daily_quota_seconds)
    inheritedQuota.push({
      label: `${groupLabel ?? "Category"} shared quota`,
      seconds: groupLimits.daily_quota_seconds,
    });

  // Unlike the budgets above, this one really cascades service -> group ->
  // entry (`policy.rs`): one session has one answer, so a category's value is
  // what its members fall back to rather than a second limit they also meet.
  const inheritedSaveGrace: InheritedValue[] = [];
  if (groupLimits?.save_grace_seconds != null)
    inheritedSaveGrace.push({
      label: `${groupLabel ?? "Category"} default`,
      seconds: groupLimits.save_grace_seconds,
    });
  inheritedSaveGrace.push({
    label: serviceSaveGrace != null ? "Service default" : "Daemon default",
    seconds: serviceSaveGrace ?? LOAD_TIME_DEFAULTS.save_grace_seconds,
  });

  const inheritedGrace: InheritedValue[] = [
    {
      label: serviceCooldownGrace ? "Service default" : "Daemon default",
      seconds: serviceCooldownGrace ?? LOAD_TIME_DEFAULTS.cooldown_min_session_seconds,
    },
  ];

  return (
    <Stack spacing={3}>
      <DurationSlider
        label="Longest single session"
        value={limits?.max_run_seconds ?? null}
        onChange={update("max_run_seconds")}
        onCommit={endGesture}
        inherited={inheritedMaxRun}
        unlimitedLabel="Unlimited"
        helperText="How long one sitting may last before the activity closes."
      />

      <DurationSlider
        label="Daily total"
        value={limits?.daily_quota_seconds ?? null}
        onChange={update("daily_quota_seconds")}
        onCommit={endGesture}
        inherited={inheritedQuota}
        unlimitedLabel="Unlimited"
        helperText={
          subject.kind === "group"
            ? "Shared across every activity in this category — once spent, they all disappear."
            : "Resets at local midnight."
        }
      />

      <DurationSlider
        label="Cooldown after a session"
        value={limits?.cooldown_seconds ?? null}
        onChange={update("cooldown_seconds")}
        onCommit={endGesture}
        unlimitedLabel="None"
        helperText="How long before this can be started again."
      />

      {(limits?.cooldown_seconds ?? 0) > 0 && (
        <DurationSlider
          label="Minimum session before a cooldown starts"
          value={limits?.cooldown_min_session_seconds ?? null}
          onChange={update("cooldown_min_session_seconds")}
          onCommit={endGesture}
          inherited={inheritedGrace}
          unlimitedLabel="Always start the cooldown"
          helperText="A session shorter than this leaves the cooldown alone, so an activity that crashes on launch does not lock anyone out."
        />
      )}

      <DurationSlider
        label="Time to save when the schedule closes"
        value={limits?.save_grace_seconds ?? null}
        onChange={update("save_grace_seconds")}
        onCommit={endGesture}
        inherited={inheritedSaveGrace}
        unlimitedLabel="Close immediately"
        helperText="If the device wakes from sleep after this activity's hours have passed, how long it stays open — with a warning — so nothing in progress is lost."
      />

      {!limits && (
        <Typography variant="caption" color="text.secondary">
          {subject.kind === "group"
            ? "Nothing set here yet — this category adds no limits of its own, so each member is bounded only by its own settings and the service defaults."
            : "Nothing set here yet — this activity uses whatever the category and service defaults say."}
        </Typography>
      )}
    </Stack>
  );
}
