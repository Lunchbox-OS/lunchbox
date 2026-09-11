/**
 * A text field that shows the keystroke it was just given.
 *
 * Every control here is controlled by the projection, and the projection is
 * re-derived from the document 120ms after an edit (`ConfigDocProvider`). A
 * plain `value={...}` field therefore cannot show a character until the whole
 * config has been re-parsed, and typing at any speed fights a value that is one
 * or more keystrokes behind — which is what made the path fields feel slow.
 *
 * So what is being typed is kept here, locally, and handed onward on every
 * keystroke as before: the document still updates live, the TOML pane and the
 * validator still keep up on their own debounce, and the cursor never waits for
 * any of it. The draft is dropped on blur, at which point the projection has
 * long since caught up and is authoritative again — the same shape
 * `DurationField` uses for a different reason.
 */
import { useState } from "react";
import TextField, { type TextFieldProps } from "@mui/material/TextField";

type Props = Omit<TextFieldProps, "value" | "onChange"> & {
  value: string;
  onChange: (value: string) => void;
};

export function DraftTextField({ value, onChange, onBlur, ...rest }: Props) {
  const [draft, setDraft] = useState<string | null>(null);

  return (
    <TextField
      {...rest}
      value={draft ?? value}
      onChange={(e) => {
        setDraft(e.target.value);
        onChange(e.target.value);
      }}
      onBlur={(e) => {
        setDraft(null);
        onBlur?.(e);
      }}
    />
  );
}
