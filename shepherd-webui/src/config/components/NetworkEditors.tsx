/**
 * Internet requirement, firewall rules, and the supervised-browser policy.
 *
 * These are the parts of the schema with no geometry — lists of CIDRs and URL
 * patterns — so they stay forms. The one thing worth saying in the UI is how
 * the two layers relate: the firewall is enforced by systemd at the IP layer
 * and cannot match hostnames, so hostname allowlisting has to come from the
 * browser policy.
 */
import Alert from "@mui/material/Alert";
import FormControlLabel from "@mui/material/FormControlLabel";
import MenuItem from "@mui/material/MenuItem";
import Stack from "@mui/material/Stack";
import Switch from "@mui/material/Switch";
import TextField from "@mui/material/TextField";
import Typography from "@mui/material/Typography";
import { useFields } from "../doc/useFields";
import type {
  RawBrowserConfig,
  RawEntryInternet,
  RawFirewallConfig,
} from "../model/config.generated";
import { Section } from "./Section";
import { StringListEditor } from "./StringListEditor";
import { FIELD_DEFAULTS } from "../model/field-defaults.generated";

export function InternetEditor({
  path,
  value,
}: {
  path: string;
  value: RawEntryInternet | null | undefined;
}) {
  const f = useFields(path);
  return (
    <Section
      title="Internet"
      description="Hide this activity while the device is offline."
      present={value != null}
      onTogglePresent={(on) =>
        on ? f.setTable("", { required: true }) : f.setField("", undefined)
      }
    >
      <Stack spacing={2}>
        <FormControlLabel
          control={
            <Switch
              checked={value?.required ?? FIELD_DEFAULTS.RawEntryInternet.required}
              onChange={(e) => f.setField("required", e.target.checked)}
            />
          }
          label="Requires an internet connection"
        />
        <FormControlLabel
          control={
            <Switch
              checked={value?.forward_check ?? FIELD_DEFAULTS.RawEntryInternet.forward_check}
              onChange={(e) => f.setField("forward_check", e.target.checked)}
            />
          }
          label="Tell the activity about the check too"
        />
        <Typography variant="caption" color="text.secondary" sx={{ mt: -1 }}>
          Media activities in browse mode poll the target and hide items with no
          local source while it fails, rather than showing tiles that error on
          tap. Turn off to launch without a check.
        </Typography>
        <TextField
          size="small"
          label="Connectivity check (optional override)"
          value={value?.check ?? ""}
          placeholder="https://example.com or tcp://1.1.1.1:53"
          onChange={(e) => f.setField("check", e.target.value)}
          helperText="Falls back to [service.internet].check when empty."
        />
      </Stack>
    </Section>
  );
}

export function FirewallEditor({
  path,
  value,
}: {
  path: string;
  value: RawFirewallConfig | null | undefined;
}) {
  const f = useFields(path);
  const policy = value?.default ?? FIELD_DEFAULTS.RawFirewallConfig.default;

  return (
    <Section
      title="Firewall"
      description="Per-session IP filtering, applied by systemd to the activity's scope."
      present={value != null}
      onTogglePresent={(on) =>
        on ? f.setTable("", { default: "deny", allow: [], deny: [] }) : f.setField("", undefined)
      }
    >
      <Stack spacing={2}>
        <TextField
          select
          size="small"
          label="Default policy"
          value={policy}
          onChange={(e) => f.setField("default", e.target.value)}
          sx={{ maxWidth: 260 }}
        >
          <MenuItem value="deny">Deny everything except the allow list</MenuItem>
          <MenuItem value="allow">Allow everything except the deny list</MenuItem>
        </TextField>

        <StringListEditor
          label="Allow"
          values={value?.allow ?? []}
          onChange={(allow) => f.setField("allow", allow)}
          placeholder="192.168.0.0/16, localhost, link-local, multicast, any"
          emptyText={
            policy === "deny"
              ? "Nothing allowed — this activity gets no network at all."
              : undefined
          }
        />
        <StringListEditor
          label="Deny"
          values={value?.deny ?? []}
          onChange={(deny) => f.setField("deny", deny)}
          placeholder="10.0.0.0/8"
          helperText="Applied after the allow list."
        />

        <Alert severity="info">
          Rules match IP addresses, not hostnames — the kernel never sees the name a
          request was made to. Pair this with the browser policy's URL allowlist when you
          need to restrict by site.
        </Alert>
      </Stack>
    </Section>
  );
}

export function BrowserEditor({
  path,
  value,
}: {
  path: string;
  value: RawBrowserConfig | null | undefined;
}) {
  const f = useFields(path);

  return (
    <Section
      title="Supervised browser"
      description="Chromium enterprise policy and profile, materialized at launch."
      present={value != null}
      onTogglePresent={(on) =>
        on
          ? f.setTable("", { profile_id: "browser", mode: "kiosk" })
          : f.setField("", undefined)
      }
    >
      <Stack spacing={2}>
        <Stack direction="row" spacing={2}>
          <TextField
            size="small"
            label="Profile id"
            required
            value={value?.profile_id ?? ""}
            onChange={(e) => f.setField("profile_id", e.target.value)}
            helperText="Activities sharing an id share cookies and logins."
          />
          <TextField
            select
            size="small"
            label="Window mode"
            value={value?.mode ?? FIELD_DEFAULTS.RawBrowserConfig.mode}
            onChange={(e) => f.setField("mode", e.target.value)}
            sx={{ minWidth: 180 }}
          >
            <MenuItem value="kiosk">Kiosk — no tabs or address bar</MenuItem>
            <MenuItem value="app">App — same as kiosk</MenuItem>
            <MenuItem value="windowed">Windowed — a normal browser</MenuItem>
          </TextField>
        </Stack>

        <TextField
          size="small"
          label="Start URL"
          value={value?.start_url ?? ""}
          placeholder="https://example.com"
          onChange={(e) => f.setField("start_url", e.target.value)}
        />

        <StringListEditor
          label="URL allowlist"
          values={value?.url_allowlist ?? []}
          onChange={(v) => f.setField("url_allowlist", v)}
          placeholder="example.com"
          emptyText="Empty means every URL is permitted, subject to the blocklist."
        />
        <StringListEditor
          label="URL blocklist"
          values={value?.url_blocklist ?? []}
          onChange={(v) => f.setField("url_blocklist", v)}
          placeholder="*"
          helperText="Applied after the allowlist."
        />

        <Stack>
          <FormControlLabel
            control={
              <Switch
                checked={value?.disable_dev_tools ?? FIELD_DEFAULTS.RawBrowserConfig.disable_dev_tools}
                onChange={(e) => f.setField("disable_dev_tools", e.target.checked)}
              />
            }
            label="Disable DevTools"
          />
          <FormControlLabel
            control={
              <Switch
                checked={value?.disable_incognito ?? FIELD_DEFAULTS.RawBrowserConfig.disable_incognito}
                onChange={(e) => f.setField("disable_incognito", e.target.checked)}
              />
            }
            label="Disable incognito"
          />
          <FormControlLabel
            control={
              <Switch
                checked={value?.disable_extensions ?? FIELD_DEFAULTS.RawBrowserConfig.disable_extensions}
                onChange={(e) => f.setField("disable_extensions", e.target.checked)}
              />
            }
            label="Block extension installation"
          />
          <FormControlLabel
            control={
              <Switch
                checked={value?.wipe_on_exit ?? FIELD_DEFAULTS.RawBrowserConfig.wipe_on_exit}
                onChange={(e) => f.setField("wipe_on_exit", e.target.checked)}
              />
            }
            label="Wipe the profile when the session ends"
          />
        </Stack>

        <Typography variant="caption" color="text.secondary">
          Pair with <code>kind = flatpak</code> pointing at Chrome, and usually a firewall
          block above.
        </Typography>
      </Stack>
    </Section>
  );
}
