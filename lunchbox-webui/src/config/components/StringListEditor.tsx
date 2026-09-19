/**
 * A list of strings — command arguments, firewall rules, URL patterns.
 *
 * One row per value with an add button, rather than a comma-separated field:
 * several of these carry values that legitimately contain commas and spaces.
 */
import Box from "@mui/material/Box";
import Button from "@mui/material/Button";
import IconButton from "@mui/material/IconButton";
import Stack from "@mui/material/Stack";
import TextField from "@mui/material/TextField";
import Typography from "@mui/material/Typography";
import AddIcon from "@mui/icons-material/Add";
import DeleteIcon from "@mui/icons-material/DeleteOutlined";

interface Props {
  label: string;
  values: string[];
  onChange: (values: string[]) => void;
  placeholder?: string;
  helperText?: string;
  emptyText?: string;
}

export function StringListEditor({
  label,
  values,
  onChange,
  placeholder,
  helperText,
  emptyText,
}: Props) {
  const update = (i: number, value: string) =>
    onChange(values.map((v, j) => (j === i ? value : v)));

  return (
    <Box>
      <Stack direction="row" sx={{ alignItems: "center", justifyContent: "space-between" }}>
        <Typography variant="body2" sx={{ fontWeight: 600 }}>
          {label}
        </Typography>
        <Button size="small" startIcon={<AddIcon />} onClick={() => onChange([...values, ""])}>
          Add
        </Button>
      </Stack>
      {helperText && (
        <Typography variant="caption" color="text.secondary" sx={{ display: "block" }}>
          {helperText}
        </Typography>
      )}
      {values.length === 0 && emptyText && (
        <Typography variant="caption" color="text.secondary" sx={{ display: "block", mt: 0.5 }}>
          {emptyText}
        </Typography>
      )}
      <Stack spacing={1} sx={{ mt: 1 }}>
        {values.map((value, i) => (
          <Stack key={i} direction="row" spacing={1} sx={{ alignItems: "center" }}>
            <TextField
              size="small"
              fullWidth
              value={value}
              placeholder={placeholder}
              onChange={(e) => update(i, e.target.value)}
              aria-label={`${label} ${i + 1}`}
            />
            <IconButton
              size="small"
              aria-label={`Remove ${label} ${i + 1}`}
              onClick={() => onChange(values.filter((_, j) => j !== i))}
            >
              <DeleteIcon fontSize="small" />
            </IconButton>
          </Stack>
        ))}
      </Stack>
    </Box>
  );
}
