/**
 * Everything an activity and a category have in common, in tabs.
 *
 * `RawGroup`'s fields are a strict subset of `RawEntry`'s — `id`, `label`,
 * `availability`, `limits`, `tokens`, and nothing a category has that an
 * activity does not. So this renders that subset for either subject, and
 * activities extend it through the slots.
 *
 * That is not just deduplication. When the two had separate detail views,
 * `[groups.tokens]` was simply forgotten: a real, enforced feature the editor
 * could not reach, because nothing made its absence visible. Rendering both
 * subjects through one component makes the whole class of omission structural
 * rather than something to remember — a field added to `RawGroup` is added
 * here, and both get it.
 *
 * The one asymmetry is `warnings`, which only `RawEntry` has. It arrives as an
 * explicit slot rather than a `subject.kind` branch, so the reason is legible
 * at the call site.
 */
import { useState, type ReactNode } from "react";
import Alert from "@mui/material/Alert";
import Stack from "@mui/material/Stack";
import Tab from "@mui/material/Tab";
import Tabs from "@mui/material/Tabs";
import TextField from "@mui/material/TextField";
import { useFields } from "../doc/useFields";
import { subjectPath, type Subject } from "../doc/patches";
import { useConfigDoc } from "../doc/ConfigDocProvider";
import type { RawConfig } from "../model/config.generated";
import { issuesForEntry, issuesForGroup } from "../model/report";
import { IssueList } from "./IssueList";
import { LimitsEditor } from "./LimitsEditor";
import { ScheduleEditor } from "./ScheduleEditor";
import { TokensEditor } from "./TokensEditor";

export interface SubjectTab {
  key: string;
  label: ReactNode;
  content: ReactNode;
}

interface Props {
  subject: Subject;
  config: RawConfig;
  /** Subject-specific content on the Basics tab, below the shared label field. */
  basics?: ReactNode;
  /**
   * Extra content on the Limits tab. Activities pass their warning thresholds
   * here; `RawGroup` has no `warnings` field, so categories pass nothing.
   */
  limitsExtra?: ReactNode;
  /** Tabs beyond the shared three. Activities add "Advanced". */
  extraTabs?: SubjectTab[];
}

export function SubjectDetail({
  subject,
  config,
  basics,
  limitsExtra,
  extraTabs = [],
}: Props) {
  const { report } = useConfigDoc();
  const [tab, setTab] = useState<string>("basics");
  const f = useFields(subjectPath(subject));

  const isGroup = subject.kind === "group";
  const entry = config.entries?.find((e) => e.id === subject.id);
  const group = isGroup
    ? config.groups?.find((g) => g.id === subject.id)
    : config.groups?.find((g) => g.id === entry?.group);
  const self = isGroup ? group : entry;

  const issues = isGroup
    ? issuesForGroup(report, subject.id)
    : issuesForEntry(report, subject.id);

  return (
    <Stack spacing={2}>
      {issues.length > 0 && <IssueList report={report} compact />}

      <Tabs value={tab} onChange={(_, v) => setTab(v as string)} variant="scrollable">
        <Tab value="basics" label="Basics" />
        <Tab value="schedule" label="Schedule" />
        <Tab value="limits" label="Limits" />
        {extraTabs.map((t) => (
          <Tab key={t.key} value={t.key} label={t.label} />
        ))}
      </Tabs>

      {tab === "basics" && (
        <Stack spacing={2}>
          <TextField
            size="small"
            label="Label"
            value={self?.label ?? ""}
            onChange={(e) => f.setField("label", e.target.value)}
            sx={{ maxWidth: 400 }}
            helperText={
              isGroup
                ? "Used when explaining why one of its activities is unavailable."
                : "What the child sees on the tile."
            }
          />
          {basics}
        </Stack>
      )}

      {tab === "schedule" && (
        <Stack spacing={2}>
          {isGroup ? (
            <Alert severity="info">
              This schedule applies on top of each member's own. Both must allow a moment
              for an activity to appear.
            </Alert>
          ) : (
            group && (
              <Alert severity="info">
                <strong>{group.label}</strong> has its own schedule. Both must allow a
                moment for this activity to appear, so the effective availability is the
                overlap — outlined on the grid.
              </Alert>
            )
          )}
          <ScheduleEditor subject={subject} availability={self?.availability} />
        </Stack>
      )}

      {tab === "limits" && (
        <Stack spacing={4}>
          <LimitsEditor
            subject={subject}
            limits={self?.limits}
            serviceMaxRun={config.service?.default_max_run_seconds}
            serviceCooldownGrace={config.service?.cooldown_min_session_seconds}
            groupLimits={isGroup ? undefined : group?.limits}
            groupLabel={isGroup ? undefined : group?.label}
          />

          {limitsExtra}

          <TokensEditor subject={subject} tokens={self?.tokens} config={config} />
        </Stack>
      )}

      {extraTabs.map((t) => (tab === t.key ? <div key={t.key}>{t.content}</div> : null))}
    </Stack>
  );
}
