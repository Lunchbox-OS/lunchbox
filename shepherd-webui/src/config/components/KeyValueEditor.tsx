/**
 * A string map — environment variables, mostly.
 *
 * Renaming a key rebuilds the map to preserve order, so editing `PATH` does not
 * send it to the bottom of the list mid-keystroke.
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
  values: Record<string, string>;
  onChange: (values: Record<string, string>) => void;
}

export function KeyValueEditor({ label, values, onChange }: Props) {
  const entries = Object.entries(values);

  const rename = (index: number, key: string) => {
    const next: Record<string, string> = {};
    entries.forEach(([k, v], i) => {
      next[i === index ? key : k] = v;
    });
    onChange(next);
  };

  const setValue = (key: string, value: string) => onChange({ ...values, [key]: value });

  const remove = (key: string) => {
    const next = { ...values };
    delete next[key];
    onChange(next);
  };

  return (
    <Box>
      <Stack direction="row" sx={{ alignItems: "center", justifyContent: "space-between" }}>
        <Typography variant="body2" sx={{ fontWeight: 600 }}>
          {label}
        </Typography>
        <Button
          size="small"
          startIcon={<AddIcon />}
          onClick={() => onChange({ ...values, "": "" })}
          disabled={"" in values}
        >
          Add
        </Button>
      </Stack>
      <Stack spacing={1} sx={{ mt: 1 }}>
        {entries.map(([key, value], i) => (
          <Stack key={i} direction="row" spacing={1} sx={{ alignItems: "center" }}>
            <TextField
              size="small"
              label="Name"
              value={key}
              onChange={(e) => rename(i, e.target.value)}
              sx={{ width: "40%" }}
            />
            <TextField
              size="small"
              label="Value"
              value={value}
              onChange={(e) => setValue(key, e.target.value)}
              sx={{ flex: 1 }}
            />
            <IconButton size="small" aria-label={`Remove ${key}`} onClick={() => remove(key)}>
              <DeleteIcon fontSize="small" />
            </IconButton>
          </Stack>
        ))}
      </Stack>
    </Box>
  );
}
