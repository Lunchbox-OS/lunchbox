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
import Checkbox from "@mui/material/Checkbox";
import Chip from "@mui/material/Chip";
import FormControlLabel from "@mui/material/FormControlLabel";
import Stack from "@mui/material/Stack";
import Switch from "@mui/material/Switch";
import TextField from "@mui/material/TextField";
import Typography from "@mui/material/Typography";
import Box from "@mui/material/Box";
import { useFields } from "../doc/useFields";
import { subjectPath, type Subject } from "../doc/patches";
import type { RawConfig, RawTokens } from "../model/config.generated";
import { tokenSources, type TokenSource } from "../model/tokenSources";
import { DurationField } from "./DurationField";
import { Section } from "./Section";
import { TokenGraph } from "./TokenGraph";

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
  const options = tokenSources(config, subject);

  const selected = (tokens?.from ?? []).map(
    (v) =>
      options.find((o) => o.value === v) ??
      ({ value: v, label: v, kind: "entry" } satisfies TokenSource),
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
          // A gate usually draws on several sources, and reopening the list
          // after every pick makes choosing three of them four interactions
          // instead of one.
          disableCloseOnSelect
          // `tokenSources` returns categories first, which is what grouping
          // requires — MUI groups consecutive runs rather than sorting.
          groupBy={(o) => (o.kind === "group" ? "Categories" : "Activities")}
          renderOption={(props, option, { selected: isSelected }) => {
            // In MUI 9 the key arrives inside props; spreading it onto the li
            // would hand React a key through props rather than as a key.
            const { key, ...liProps } = props;
            return (
              <Box component="li" key={key} {...liProps}>
                <Checkbox size="small" checked={isSelected} sx={{ mr: 1, p: 0.5 }} />
                {option.label}
              </Box>
            );
          }}
          renderValue={(value, getItemProps) =>
            value.map((option, index) => (
              <Chip
                {...getItemProps({ index })}
                key={option.value}
                size="small"
                label={
                  option.kind === "group" ? `${option.label} (category)` : option.label
                }
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
