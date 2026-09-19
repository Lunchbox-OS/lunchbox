// The connection drawer.
//
// Since issue #156 the token field is no longer how a person signs in — that
// is a password or an approval on the paired phone, and what it yields is an
// `HttpOnly` cookie this page cannot read. The field survives for the one case
// a cookie cannot serve: pointing this UI at a *different* origin, where the
// browser will not send this origin's cookie, and for `npm run dev` against a
// real device.

import { useState } from "react";
import Box from "@mui/material/Box";
import Button from "@mui/material/Button";
import Stack from "@mui/material/Stack";
import SwipeableDrawer from "@mui/material/SwipeableDrawer";
import TextField from "@mui/material/TextField";
import Typography from "@mui/material/Typography";

export function ConnectionSettings({ onClose }: { onClose: () => void }) {
  const [apiBase, setApiBase] = useState(() => localStorage.getItem("apiBase") ?? "");
  const [token, setToken] = useState(() => localStorage.getItem("apiToken") ?? "");

  const handleSave = () => {
    if (apiBase.trim()) {
      localStorage.setItem("apiBase", apiBase.trim());
    } else {
      localStorage.removeItem("apiBase");
    }
    if (token.trim()) {
      localStorage.setItem("apiToken", token.trim());
    } else {
      localStorage.removeItem("apiToken");
    }
    onClose();
    window.location.reload();
  };

  return (
    <SwipeableDrawer
      anchor="bottom"
      open
      onOpen={() => {}}
      onClose={onClose}
      disableSwipeToOpen
      slotProps={{
        paper: {
          sx: {
            borderTopLeftRadius: 16,
            borderTopRightRadius: 16,
            px: 2,
            pt: 1,
            pb: "max(24px, env(safe-area-inset-bottom))",
            maxWidth: 640,
            mx: "auto",
          },
        },
      }}
    >
      {/* drag handle */}
      <Box
        sx={{ width: 40, height: 4, borderRadius: 2, bgcolor: "divider", mx: "auto", mb: 2 }}
      />

      <Typography variant="h6" gutterBottom sx={{ fontWeight: 700 }}>
        Connection
      </Typography>

      <Stack spacing={2} sx={{ mt: 1 }}>
        <TextField
          label="API Server URL"
          type="url"
          placeholder="http://192.168.1.10:8080"
          value={apiBase}
          onChange={(e) => setApiBase(e.target.value)}
          helperText="Leave blank to use the current host"
          fullWidth
          slotProps={{ htmlInput: { spellCheck: false, autoComplete: "off" } }}
        />

        <TextField
          label="Machine token (optional)"
          type="password"
          placeholder="service.management_api.auth_token"
          value={token}
          onChange={(e) => setToken(e.target.value)}
          helperText="Only needed when the server URL above points at another
            origin — a browser will not send this page's session cookie there.
            Signing in normally needs nothing here."
          fullWidth
          slotProps={{ htmlInput: { autoComplete: "new-password" } }}
        />

        <Box sx={{ display: "flex", gap: 1, justifyContent: "flex-end", pt: 1 }}>
          <Button variant="text" onClick={onClose}>Cancel</Button>
          <Button variant="contained" onClick={handleSave}>Save &amp; Reload</Button>
        </Box>
      </Stack>
    </SwipeableDrawer>
  );
}
