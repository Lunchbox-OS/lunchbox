/**
 * A duration in seconds, edited as something a person would say.
 *
 * The config is entirely in seconds — `max_run_seconds`, `daily_quota_seconds`,
 * `seconds_before` — and nobody thinks in seconds. This accepts "1h 30m",
 * "90", "1:30" and prints "1h 30m".
 *
 * Kept as free text while focused so a half-typed value is not reinterpreted on
 * every keystroke; committed on blur or Enter.
 */
import { useEffect, useState } from "react";
import TextField from "@mui/material/TextField";
import InputAdornment from "@mui/material/InputAdornment";
import { formatDurationHuman, parseDurationHuman } from "../../shared/duration";

interface Props {
  label?: string;
  /** Seconds, or null when the setting is absent. */
  value: number | null;
  onChange: (seconds: number | null) => void;
  /** What to show when `value` is null — usually the inherited or default value. */
  placeholder?: string;
  helperText?: string;
  disabled?: boolean;
  size?: "small" | "medium";
  fullWidth?: boolean;
  /** Allow clearing the field back to "not set". */
  clearable?: boolean;
}

export function DurationField({
  label,
  value,
  onChange,
  placeholder,
  helperText,
  disabled,
  size = "small",
  fullWidth,
  clearable = true,
}: Props) {
  const [draft, setDraft] = useState<string | null>(null);
  const display = draft ?? (value === null ? "" : formatDurationHuman(value));

  // Adopt external changes (undo, a patch from elsewhere) while not editing.
  useEffect(() => {
    if (draft === null) return;
  }, [value, draft]);

  const commit = () => {
    if (draft === null) return;
    const trimmed = draft.trim();
    if (trimmed === "") {
      if (clearable) onChange(null);
      setDraft(null);
      return;
    }
    const parsed = parseDurationHuman(trimmed);
    // Unparseable input reverts rather than silently becoming zero.
    if (parsed !== null) onChange(parsed);
    setDraft(null);
  };

  return (
    <TextField
      label={label}
      value={display}
      placeholder={placeholder}
      helperText={helperText}
      disabled={disabled}
      size={size}
      fullWidth={fullWidth}
      onChange={(e) => setDraft(e.target.value)}
      onBlur={commit}
      onKeyDown={(e) => {
        if (e.key === "Enter") {
          commit();
          (e.target as HTMLInputElement).blur();
        }
        if (e.key === "Escape") setDraft(null);
      }}
      slotProps={{
        input: {
          endAdornment: (
            <InputAdornment position="end" sx={{ opacity: 0.6, fontSize: 12 }}>
              {value === null ? "" : `${value}s`}
            </InputAdornment>
          ),
        },
      }}
    />
  );
}
