/**
 * A titled block in a settings page, collapsed by default when it is off.
 *
 * Most of the config is optional, and a page that renders every table expanded
 * buries the two settings someone came to change. A section that is not present
 * in the file shows as a switch to add it.
 */
import { useState, type ReactNode } from "react";
import Box from "@mui/material/Box";
import Collapse from "@mui/material/Collapse";
import Divider from "@mui/material/Divider";
import IconButton from "@mui/material/IconButton";
import Stack from "@mui/material/Stack";
import Switch from "@mui/material/Switch";
import Typography from "@mui/material/Typography";
import ExpandMoreIcon from "@mui/icons-material/ExpandMore";

interface Props {
  title: string;
  description?: string;
  children: ReactNode;
  /** When given, the section is optional and this toggles its presence. */
  present?: boolean;
  onTogglePresent?: (present: boolean) => void;
  defaultExpanded?: boolean;
}

export function Section({
  title,
  description,
  children,
  present,
  onTogglePresent,
  defaultExpanded,
}: Props) {
  const optional = present !== undefined;
  const [expanded, setExpanded] = useState(defaultExpanded ?? (optional ? present : true));
  const open = expanded && (!optional || present);

  return (
    <Box sx={{ py: 1 }}>
      <Stack direction="row" spacing={1} sx={{ alignItems: "center" }}>
        {optional && (
          <Switch
            size="small"
            checked={present}
            onChange={(e) => {
              onTogglePresent?.(e.target.checked);
              if (e.target.checked) setExpanded(true);
            }}
            slotProps={{ input: { "aria-label": `Enable ${title}` } }}
          />
        )}
        <Box sx={{ flex: 1, cursor: "pointer" }} onClick={() => setExpanded((v) => !v)}>
          <Typography variant="subtitle2">{title}</Typography>
          {description && (
            <Typography variant="caption" color="text.secondary">
              {description}
            </Typography>
          )}
        </Box>
        <IconButton
          size="small"
          onClick={() => setExpanded((v) => !v)}
          aria-label={open ? `Collapse ${title}` : `Expand ${title}`}
          sx={{ transform: open ? "rotate(180deg)" : "none", transition: "transform .15s" }}
        >
          <ExpandMoreIcon fontSize="small" />
        </IconButton>
      </Stack>
      <Collapse in={open} unmountOnExit>
        <Box sx={{ pt: 1.5, pl: optional ? 5 : 0 }}>{children}</Box>
      </Collapse>
      <Divider sx={{ mt: 1 }} />
    </Box>
  );
}
