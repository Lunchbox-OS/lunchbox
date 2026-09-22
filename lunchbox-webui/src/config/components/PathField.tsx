/**
 * A text field for a path, with a "Browse…" button when there is a device to
 * browse (issue #186).
 *
 * `DraftTextField` with an adornment, rather than a control of its own: the
 * field is still the interface. Every path in the schema has a spelling the
 * picker cannot produce — `library` takes a YouTube playlist URL, `icon` takes
 * a theme name, and any of them can name a file that is not on the device yet —
 * so the button is an accelerator for the common case and never a gate.
 *
 * With no picker in context it renders exactly the field it replaced, down to
 * the adornment being absent rather than disabled. That is what makes it safe
 * in the standalone bundle and on a device with the file manager switched off.
 */
import { useState } from "react";
import IconButton from "@mui/material/IconButton";
import InputAdornment from "@mui/material/InputAdornment";
import Tooltip from "@mui/material/Tooltip";
import FolderOpenIcon from "@mui/icons-material/FolderOpen";
import { DraftTextField } from "./DraftTextField";
import { useFilePicker, type PathKind } from "../pick/FilePicker";

interface Props {
  label: string;
  value: string;
  onChange: (value: string) => void;
  /** What a valid answer is, and what to call it in the dialog's title. */
  picks: { kind: PathKind; what: string; showHidden?: boolean };
  required?: boolean;
  placeholder?: string;
  helperText?: string;
}

export function PathField({
  label,
  value,
  onChange,
  picks,
  required,
  placeholder,
  helperText,
}: Props) {
  const picker = useFilePicker();
  // Only to keep the button from being pressed twice while a dialog is
  // already up; the dialog itself is modal.
  const [picking, setPicking] = useState(false);

  const browse = async () => {
    if (!picker) return;
    setPicking(true);
    try {
      const picked = await picker.pick({ ...picks, start: value || undefined });
      // Null is a cancel, and a cancel must not clear the field.
      if (picked !== null) onChange(picked);
    } finally {
      setPicking(false);
    }
  };

  return (
    <DraftTextField
      size="small"
      label={label}
      required={required}
      value={value}
      onChange={onChange}
      placeholder={placeholder}
      helperText={helperText}
      slotProps={
        picker
          ? {
              input: {
                endAdornment: (
                  <InputAdornment position="end">
                    <Tooltip title={`Choose ${picks.what} on this device`}>
                      {/* A span, because a disabled button dispatches no
                          events and MUI's tooltip listens on the child. */}
                      <span>
                        <IconButton
                          size="small"
                          edge="end"
                          disabled={picking}
                          aria-label={`Browse for ${picks.what}`}
                          onClick={browse}
                        >
                          <FolderOpenIcon fontSize="small" />
                        </IconButton>
                      </span>
                    </Tooltip>
                  </InputAdornment>
                ),
              },
            }
          : undefined
      }
    />
  );
}
