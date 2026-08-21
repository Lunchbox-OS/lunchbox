/**
 * Token gates: time earned on some activities unlocking another.
 *
 * Configured on the *target* — the thing that stays locked — which reads
 * backwards until you see it drawn. Both activities and categories can carry a
 * gate (`[entries.tokens]` and `[groups.tokens]`), and a category's gate
 * unlocks every member at once.
 *
 * `from` takes entry ids, or a category as `group:<id>`, so it needs a picker
 * over the current document rather than a text field. The picker also has to
 * know what the validator forbids, because "a thing cannot unlock itself" takes
 * four forms depending on who owns the gate — see `validate_tokens` in
 * `crates/shepherd-config/src/validation.rs`.
 */
import Alert from "@mui/material/Alert";
import Autocomplete from "@mui/material/Autocomplete";
import Chip from "@mui/material/Chip";
import FormControlLabel from "@mui/material/FormControlLabel";
import Stack from "@mui/material/Stack";
import Switch from "@mui/material/Switch";
import TextField from "@mui/material/TextField";
import Typography from "@mui/material/Typography";
import { useFields } from "../doc/useFields";
import { subjectPath, type Subject } from "../doc/patches";
import type { RawConfig, RawTokens } from "../model/config.generated";
import { DurationField } from "./DurationField";
import { Section } from "./Section";
import { TokenGraph } from "./TokenGraph";

const GROUP_PREFIX = "group:";

interface Option {
  value: string;
  label: string;
  kind: "entry" | "group";
}

/**
 * Sources this subject is allowed to earn from.
 *
 * An activity cannot be unlocked by itself, nor by the category it belongs to
 * (its own time would count toward the group total). A category cannot be
 * unlocked by itself, nor by any of its members.
 */
function sourceOptions(config: RawConfig, subject: Subject): Option[] {
  const groups = config.groups ?? [];
  const entries = config.entries ?? [];

  if (subject.kind === "group") {
    return [
      ...groups
        .filter((g) => g.id !== subject.id)
        .map((g) => ({
          value: `${GROUP_PREFIX}${g.id}`,
          label: `${g.label} (whole category)`,
          kind: "group" as const,
        })),
      ...entries
        .filter((e) => e.group !== subject.id)
        .map((e) => ({ value: e.id, label: e.label, kind: "entry" as const })),
    ];
  }

  const ownGroup = entries.find((e) => e.id === subject.id)?.group;
  return [
    ...groups
      .filter((g) => g.id !== ownGroup)
      .map((g) => ({
        value: `${GROUP_PREFIX}${g.id}`,
        label: `${g.label} (whole category)`,
        kind: "group" as const,
      })),
    ...entries
      .filter((e) => e.id !== subject.id)
      .map((e) => ({ value: e.id, label: e.label, kind: "entry" as const })),
  ];
}

export function TokensEditor({
  subject,
  tokens,
  config,
}: {
  subject: Subject;
  tokens: RawTokens | null | undefined;
  config: RawConfig;
}) {
  const f = useFields(subjectPath(subject, "tokens"));
  const isGroup = subject.kind === "group";
  const options = sourceOptions(config, subject);

  const selected = (tokens?.from ?? []).map(
    (v) =>
      options.find((o) => o.value === v) ?? {
        value: v,
        label: v,
        kind: "entry" as const,
      },
  );

  return (
    <Section
      title="Token gate"
      description={
        isGroup
          ? "Keep every activity in this category locked until time has been spent elsewhere."
          : "Stay locked until time has been spent on something else."
      }
      present={tokens != null}
      onTogglePresent={(on) => (on ? f.setTable("", { from: [] }) : f.setField("", undefined))}
    >
      <Stack spacing={2}>
        <Autocomplete
          multiple
          size="small"
          options={options}
          value={selected}
          isOptionEqualToValue={(a, b) => a.value === b.value}
          getOptionLabel={(o) => o.label}
          onChange={(_, next) => f.setField("from", next.map((o) => o.value))}
          renderValue={(value, getItemProps) =>
            value.map((option, index) => (
              <Chip
                {...getItemProps({ index })}
                key={option.value}
                size="small"
                label={option.label}
                color={option.kind === "group" ? "secondary" : "default"}
              />
            ))
          }
          renderInput={(params) => (
            <TextField
              {...params}
              label="Time on these unlocks it"
              helperText={
                isGroup
                  ? "This category's own members are not offered — a category cannot unlock itself."
                  : "Pick activities, or a whole category to count every member."
              }
            />
          )}
        />

        {(tokens?.from ?? []).length === 0 && (
          <Alert severity="warning">
            A token gate with no sources can never unlock. Add at least one, or turn the
            gate off.
          </Alert>
        )}

        <Stack direction="row" spacing={2} sx={{ flexWrap: "wrap" }} useFlexGap>
          <TextField
            size="small"
            type="number"
            label="Earn ratio"
            slotProps={{ htmlInput: { step: 0.1, min: 0 } }}
            value={tokens?.earn_ratio ?? ""}
            placeholder="1.0"
            onChange={(e) =>
              f.setField("earn_ratio", e.target.value === "" ? undefined : Number(e.target.value))
            }
            helperText="Seconds banked per second spent"
          />
          <DurationField
            label="Needed to unlock"
            value={tokens?.minimum_seconds ?? null}
            onChange={(v) => f.setField("minimum_seconds", v ?? undefined)}
            placeholder="any balance"
            helperText="Threshold to cross before it opens"
          />
          <DurationField
            label="Balance ceiling"
            value={tokens?.max_balance_seconds ?? null}
            onChange={(v) => f.setField("max_balance_seconds", v ?? undefined)}
            placeholder="unlimited"
            helperText="0 or empty means no cap"
          />
        </Stack>

        <FormControlLabel
          control={
            <Switch
              checked={tokens?.carry_over ?? false}
              onChange={(e) => f.setField("carry_over", e.target.checked)}
            />
          }
          label="Unspent balance survives midnight"
        />

        <Typography variant="caption" color="text.secondary">
          Once the threshold is crossed the gate ratchets open and stays open until the
          balance is spent to zero, so a partly-used balance never re-locks
          {isGroup ? " the category" : " the activity"}.
        </Typography>

        {(tokens?.from ?? []).length > 0 && (
          <TokenGraph config={config} highlight={subject} />
        )}
      </Stack>
    </Section>
  );
}
