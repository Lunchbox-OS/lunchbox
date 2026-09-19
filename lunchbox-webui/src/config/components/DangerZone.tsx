/**
 * A red-ruled box around settings that can take the device away from you.
 *
 * GitHub's "Danger Zone" by shape, and for the same reason: some settings on a
 * settings page are not merely important but *self-referential* — they decide
 * whether the thing you are using to change them still works afterwards. The
 * management API is Lunchbox's one such block. A config that disables it,
 * binds it to an address this browser cannot reach, or turns off the TLS a
 * non-loopback bind requires, ends the session that saved it; and on a device
 * that `harden apply` has been run against there is no SSH to go back in with.
 *
 * The border is not decoration. Everything else on this page is recoverable by
 * editing it again from the same place.
 */
import type { ReactNode } from "react";
import Box from "@mui/material/Box";
import Stack from "@mui/material/Stack";
import Typography from "@mui/material/Typography";
import WarningAmberIcon from "@mui/icons-material/WarningAmber";

interface Props {
  /** Heading inside the rule. */
  title?: string;
  /** What specifically goes wrong, in a sentence or two. */
  warning: ReactNode;
  children: ReactNode;
}

export function DangerZone({ title = "Danger zone", warning, children }: Props) {
  return (
    <Box
      sx={{
        my: 1,
        border: 1,
        borderColor: "error.main",
        borderRadius: 1,
        overflow: "hidden",
      }}
    >
      <Stack
        direction="row"
        spacing={1}
        sx={{
          alignItems: "flex-start",
          px: 2,
          py: 1.25,
          // A tint rather than a fill: this is a warning, not an error that has
          // already happened, and a solid red header reads as the latter.
          bgcolor: (theme) => theme.palette.error.main + "14",
          borderBottom: 1,
          borderColor: "error.main",
        }}
      >
        <WarningAmberIcon fontSize="small" color="error" sx={{ mt: "2px" }} />
        <Box>
          <Typography variant="subtitle2" color="error.main">
            {title}
          </Typography>
          <Typography variant="caption" color="text.secondary" component="p">
            {warning}
          </Typography>
        </Box>
      </Stack>
      <Box sx={{ px: 2 }}>{children}</Box>
    </Box>
  );
}
