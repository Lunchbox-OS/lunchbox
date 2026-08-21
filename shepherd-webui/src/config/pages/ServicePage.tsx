/**
 * Device-wide settings: `[service]` and its sub-tables.
 *
 * Ordered by how often anyone touches them — defaults and warnings first,
 * transports and filesystem paths last — rather than by their order in the
 * schema.
 */
import Alert from "@mui/material/Alert";
import Box from "@mui/material/Box";
import FormControlLabel from "@mui/material/FormControlLabel";
import Stack from "@mui/material/Stack";
import Switch from "@mui/material/Switch";
import TextField from "@mui/material/TextField";
import Typography from "@mui/material/Typography";
import { useConfigDoc } from "../doc/ConfigDocProvider";
import { insert, servicePath, set, unset } from "../doc/patches";
import { useFields } from "../doc/useFields";
import type { RawConfig } from "../model/config.generated";
import { DurationField } from "../components/DurationField";
import { BrightnessEditor, VolumeEditor } from "../components/RestrictionEditors";
import { Section } from "../components/Section";
import { StringListEditor } from "../components/StringListEditor";
import { WarningTimeline } from "../components/WarningTimeline";

export function ServicePage({ config }: { config: RawConfig }) {
  const { apply, endGesture } = useConfigDoc();
  const service = config.service ?? {};
  const f = useFields("service");

  const warningsPath = servicePath("default_warnings");
  const warnings = service.default_warnings ?? [];

  return (
    <Box>
      <Typography variant="h6" sx={{ mb: 2 }}>
        Device settings
      </Typography>

      <Stack spacing={1}>
        <Section title="Defaults" defaultExpanded>
          <Stack spacing={2} sx={{ maxWidth: 520 }}>
            <DurationField
              label="Default session length"
              value={service.default_max_run_seconds ?? null}
              onChange={(v) => f.setField("default_max_run_seconds", v ?? undefined)}
              placeholder="1h (the daemon's own default)"
              helperText="Applies to any activity that does not set its own."
              fullWidth
            />
            <DurationField
              label="Minimum session before a cooldown starts"
              value={service.cooldown_min_session_seconds ?? null}
              onChange={(v) => f.setField("cooldown_min_session_seconds", v ?? undefined)}
              placeholder="2m"
              helperText="A session shorter than this leaves the cooldown alone, so an activity that crashes on launch does not lock anyone out. Overridable per activity and per category."
              fullWidth
            />
          </Stack>
        </Section>

        <Section
          title="Default warnings"
          description="Used by any activity that does not define its own."
          present={service.default_warnings != null}
          onTogglePresent={(on) =>
            on
              ? apply(
                  set(warningsPath, [
                    { seconds_before: 300, severity: "info" },
                    { seconds_before: 60, severity: "warn" },
                    { seconds_before: 10, severity: "critical" },
                  ] as never),
                )
              : apply(unset(warningsPath))
          }
        >
          <WarningTimeline
            warnings={warnings}
            maxRunSeconds={service.default_max_run_seconds ?? 3600}
            onChange={(i, next) => {
              for (const [key, value] of Object.entries(next)) {
                const path = `${warningsPath}[${i}].${key}`;
                if (value === null || value === undefined) apply(unset(path));
                else apply(set(path, value as never), `drag:${path}`);
              }
            }}
            onCommit={endGesture}
            onAdd={(w) => apply(insert(warningsPath, w as never))}
            onRemove={(i) => apply(unset(`${warningsPath}[${i}]`))}
          />
        </Section>

        <VolumeEditor path="service.volume" value={service.volume} />
        <BrightnessEditor path="service.brightness" value={service.brightness} allowAuto />

        <Section
          title="Connectivity check"
          description="How the daemon decides whether the device is online."
          present={service.internet != null}
          onTogglePresent={(on) =>
            on
              ? apply(
                  set(servicePath("internet"), {
                    check: "https://connectivitycheck.gstatic.com/generate_204",
                    interval_seconds: 300,
                    timeout_ms: 1500,
                  } as never),
                )
              : apply(unset(servicePath("internet")))
          }
        >
          <InternetServiceEditor config={config} />
        </Section>

        <Section
          title="Steam"
          description="Behaviour of activities launched through the Steam snap."
          present={service.steam != null}
          onTogglePresent={(on) =>
            on
              ? apply(set(servicePath("steam"), { allow_risky_dismiss: false } as never))
              : apply(unset(servicePath("steam")))
          }
        >
          <SteamEditor config={config} />
        </Section>

        <Section
          title="External displays"
          description="What happens when a monitor is plugged in."
          present={service.display != null}
          onTogglePresent={(on) =>
            on
              ? apply(
                  set(servicePath("display"), {
                    docking_enabled: true,
                    mirror_audio: true,
                  } as never),
                )
              : apply(unset(servicePath("display")))
          }
        >
          <DisplayEditor config={config} />
        </Section>

        <Section
          title="Management API"
          description="The HTTP interface this editor will eventually talk to."
          present={service.management_api != null}
          onTogglePresent={(on) =>
            on
              ? apply(set(servicePath("management_api"), { enabled: true } as never))
              : apply(unset(servicePath("management_api")))
          }
        >
          <ManagementApiEditor config={config} />
        </Section>

        <Section
          title="Bluetooth management"
          description="The companion app's transport; works without any network setup."
          present={service.ble_management != null}
          onTogglePresent={(on) =>
            on
              ? apply(set(servicePath("ble_management"), { enabled: true } as never))
              : apply(unset(servicePath("ble_management")))
          }
        >
          <BleEditor config={config} />
        </Section>

        <Section
          title="Filesystem paths"
          description="Sockets, logs and data. Sensible defaults apply when unset."
        >
          <PathsEditor config={config} />
        </Section>
      </Stack>
    </Box>
  );
}

function InternetServiceEditor({ config }: { config: RawConfig }) {
  const f = useFields("service.internet");
  const v = config.service?.internet;
  return (
    <Stack spacing={2} sx={{ maxWidth: 520 }}>
      <TextField
        size="small"
        label="Check target"
        value={v?.check ?? ""}
        placeholder="https://connectivitycheck.gstatic.com/generate_204"
        onChange={(e) => f.setField("check", e.target.value)}
        helperText="An http(s) URL, or tcp://host:port."
      />
      <Stack direction="row" spacing={2}>
        <TextField
          size="small"
          type="number"
          label="Interval (s)"
          value={v?.interval_seconds ?? ""}
          onChange={(e) =>
            f.setField(
              "interval_seconds",
              e.target.value === "" ? undefined : Number(e.target.value),
            )
          }
        />
        <TextField
          size="small"
          type="number"
          label="Timeout (ms)"
          value={v?.timeout_ms ?? ""}
          onChange={(e) =>
            f.setField("timeout_ms", e.target.value === "" ? undefined : Number(e.target.value))
          }
        />
      </Stack>
    </Stack>
  );
}

function SteamEditor({ config }: { config: RawConfig }) {
  const f = useFields("service.steam");
  const v = config.service?.steam;
  return (
    <Stack spacing={2}>
      <StringListEditor
        label="Auto-dismiss interstitials"
        values={v?.auto_dismiss_interstitials ?? []}
        onChange={(list) => f.setField("auto_dismiss_interstitials", list)}
        placeholder="cloud_sync"
        helperText="Blocking modals between launch and the game starting, which a kiosk cannot show. Leave unset for a safe default set; an empty list disables the feature and never opens the debugging port."
      />
      <FormControlLabel
        control={
          <Switch
            checked={v?.allow_risky_dismiss ?? false}
            onChange={(e) => f.setField("allow_risky_dismiss", e.target.checked)}
          />
        }
        label="Allow risky interstitial kinds"
      />
      <Alert severity="warning">
        Risky kinds dismiss modals whose game cannot actually be played without missing
        hardware — the activity launches into something unusable.
      </Alert>
      <TextField
        size="small"
        type="number"
        label="Launch timeout (s)"
        value={v?.launch_timeout_seconds ?? ""}
        onChange={(e) =>
          f.setField(
            "launch_timeout_seconds",
            e.target.value === "" ? undefined : Number(e.target.value),
          )
        }
        helperText="How long to wait for the game window before giving up."
        sx={{ maxWidth: 260 }}
      />
    </Stack>
  );
}

function DisplayEditor({ config }: { config: RawConfig }) {
  const f = useFields("service.display");
  const v = config.service?.display;
  return (
    <Stack>
      <FormControlLabel
        control={
          <Switch
            checked={v?.docking_enabled ?? true}
            onChange={(e) => f.setField("docking_enabled", e.target.checked)}
          />
        }
        label="Manage external displays"
      />
      <FormControlLabel
        control={
          <Switch
            checked={v?.mirror_audio ?? true}
            onChange={(e) => f.setField("mirror_audio", e.target.checked)}
          />
        }
        label="Route audio to the external display"
      />
    </Stack>
  );
}

function ManagementApiEditor({ config }: { config: RawConfig }) {
  const f = useFields("service.management_api");
  const v = config.service?.management_api;
  return (
    <Stack spacing={2} sx={{ maxWidth: 520 }}>
      <FormControlLabel
        control={
          <Switch
            checked={v?.enabled ?? false}
            onChange={(e) => f.setField("enabled", e.target.checked)}
          />
        }
        label="Enabled"
      />
      <Stack direction="row" spacing={2}>
        <TextField
          size="small"
          type="number"
          label="Port"
          value={v?.port ?? ""}
          placeholder="7890"
          onChange={(e) =>
            f.setField("port", e.target.value === "" ? undefined : Number(e.target.value))
          }
        />
        <TextField
          size="small"
          label="Bind address"
          value={v?.bind ?? ""}
          placeholder="127.0.0.1"
          onChange={(e) => f.setField("bind", e.target.value)}
          sx={{ flex: 1 }}
        />
      </Stack>
      <TextField
        size="small"
        label="Bearer token"
        value={v?.auth_token ?? ""}
        onChange={(e) => f.setField("auth_token", e.target.value)}
        helperText="Leave empty to trust every client that can reach the port."
      />
      {v?.enabled && !v?.auth_token && v?.bind && v.bind !== "127.0.0.1" && (
        <Alert severity="warning">
          This binds beyond loopback with no token, so anything on the network can control
          the device.
        </Alert>
      )}
      <TextField
        size="small"
        type="number"
        label="Bind retry (s)"
        value={v?.bind_retry_seconds ?? ""}
        placeholder="300"
        onChange={(e) =>
          f.setField(
            "bind_retry_seconds",
            e.target.value === "" ? undefined : Number(e.target.value),
          )
        }
        helperText="How long to keep retrying when the interface is not up yet. 0 retries forever."
        sx={{ maxWidth: 260 }}
      />
    </Stack>
  );
}

function BleEditor({ config }: { config: RawConfig }) {
  const f = useFields("service.ble_management");
  const v = config.service?.ble_management;
  return (
    <Stack spacing={2} sx={{ maxWidth: 520 }}>
      <FormControlLabel
        control={
          <Switch
            checked={v?.enabled ?? false}
            onChange={(e) => f.setField("enabled", e.target.checked)}
          />
        }
        label="Enabled"
      />
      <TextField
        size="small"
        label="Advertised name"
        value={v?.device_name ?? ""}
        placeholder="shepherd"
        onChange={(e) => f.setField("device_name", e.target.value)}
        helperText="What the companion app shows when several devices are in range."
      />
      <TextField
        size="small"
        label="Bluetooth adapter"
        value={v?.adapter ?? ""}
        placeholder="DC:56:7B:1F:7D:EA"
        onChange={(e) => f.setField("adapter", e.target.value)}
        helperText="A controller address, or an interface name like hci1. Prefer the address: interface numbering tracks probe order and moves between boots."
      />
      <TextField
        size="small"
        label="Admin record path"
        value={v?.admin_record_path ?? ""}
        placeholder="<data directory>/admin.toml"
        onChange={(e) => f.setField("admin_record_path", e.target.value)}
        helperText="Where the claimed admin is persisted."
      />
      <TextField
        size="small"
        label="Factory-reset sentinel path"
        value={v?.reset_sentinel_path ?? ""}
        placeholder="<data directory>/.factory-reset-ble"
        onChange={(e) => f.setField("reset_sentinel_path", e.target.value)}
        helperText="Creating this file and restarting the daemon wipes the admin record and returns the device to unclaimed. The daemon removes it afterwards."
      />
    </Stack>
  );
}

function PathsEditor({ config }: { config: RawConfig }) {
  const f = useFields("service");
  const s = config.service ?? {};
  return (
    <Stack spacing={2} sx={{ maxWidth: 640 }}>
      <TextField
        size="small"
        label="Socket path"
        value={s.socket_path ?? ""}
        placeholder="$XDG_RUNTIME_DIR/shepherdd/shepherdd.sock"
        onChange={(e) => f.setField("socket_path", e.target.value)}
      />
      <TextField
        size="small"
        label="Data directory"
        value={s.data_dir ?? ""}
        placeholder="$XDG_DATA_HOME/shepherdd"
        onChange={(e) => f.setField("data_dir", e.target.value)}
      />
      <TextField
        size="small"
        label="Log directory"
        value={s.log_dir ?? ""}
        placeholder="$XDG_STATE_HOME/shepherdd"
        onChange={(e) => f.setField("log_dir", e.target.value)}
      />
      <FormControlLabel
        control={
          <Switch
            checked={s.capture_child_output ?? false}
            onChange={(e) => f.setField("capture_child_output", e.target.checked)}
          />
        }
        label="Capture output from launched activities"
      />
      {s.capture_child_output && (
        <TextField
          size="small"
          label="Activity log directory"
          value={s.child_log_dir ?? ""}
          placeholder="<log directory>/sessions"
          onChange={(e) => f.setField("child_log_dir", e.target.value)}
        />
      )}
    </Stack>
  );
}
