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
import Checkbox from "@mui/material/Checkbox";
import FormGroup from "@mui/material/FormGroup";
import MenuItem from "@mui/material/MenuItem";
import type {
  RawConfig,
  RawSponsorBlockCategory,
} from "../model/config.generated";
import {
  DEFAULT_HUD_ORIENTATION_LABEL,
  HUD_ORIENTATIONS,
  VERTICAL_HUD_DESCRIPTION,
} from "../model/hudOrientation";
import { DurationField } from "../components/DurationField";
import {
  BrightnessEditor,
  VolumeEditor,
} from "../components/RestrictionEditors";
import { DangerZone } from "../components/DangerZone";
import { KeyValueEditor } from "../components/KeyValueEditor";
import { Section } from "../components/Section";
import { StringListEditor } from "../components/StringListEditor";
import { WarningTimeline } from "../components/WarningTimeline";
import {
  FIELD_DEFAULTS,
  LOAD_TIME_DEFAULTS,
} from "../model/field-defaults.generated";

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
              onChange={(v) =>
                f.setField("default_max_run_seconds", v ?? undefined)
              }
              placeholder={`${secondsPlaceholder(LOAD_TIME_DEFAULTS.max_run_seconds)} (the daemon's own default)`}
              helperText="Applies to any activity that does not set its own."
              fullWidth
            />
            <DurationField
              label="Minimum session before a cooldown starts"
              value={service.cooldown_min_session_seconds ?? null}
              onChange={(v) =>
                f.setField("cooldown_min_session_seconds", v ?? undefined)
              }
              placeholder={secondsPlaceholder(
                LOAD_TIME_DEFAULTS.cooldown_min_session_seconds,
              )}
              helperText="A session shorter than this leaves the cooldown alone, so an activity that crashes on launch does not lock anyone out. Overridable per activity and per category."
              fullWidth
            />
            <DurationField
              label="Time to save when the schedule closes"
              value={service.save_grace_seconds ?? null}
              onChange={(v) => f.setField("save_grace_seconds", v ?? undefined)}
              placeholder={secondsPlaceholder(LOAD_TIME_DEFAULTS.save_grace_seconds)}
              helperText="If the device wakes from sleep after an activity's hours have passed, this is how long it stays open — with a warning — so nothing in progress is lost. Overridable per activity and per category."
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
                  set(warningsPath, [...LOAD_TIME_DEFAULTS.warnings] as never),
                )
              : apply(unset(warningsPath))
          }
        >
          <WarningTimeline
            warnings={warnings}
            maxRunSeconds={
          service.default_max_run_seconds ?? LOAD_TIME_DEFAULTS.max_run_seconds
        }
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
        <BrightnessEditor
          path="service.brightness"
          value={service.brightness}
          allowAuto
        />

        <Section
          title="Connectivity check"
          description="How the daemon decides whether the device is online."
          present={service.internet != null}
          onTogglePresent={(on) =>
            on
              ? apply(
                  // Deliberately not `LOAD_TIME_DEFAULTS`: the daemon falls
                  // back to a 10s interval, which is right for a check that
                  // gates an activity, and wrong as a starting point for one
                  // an admin just switched on. This seeds what
                  // `config.example.toml` recommends. The placeholders below
                  // still show the real fallback, for a field left empty.
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
          title="Media"
          description="Caching and background downloads for media activities."
          present={service.media != null}
          onTogglePresent={(on) =>
            on
              ? apply(set(servicePath("media"), { prefetch: true } as never))
              : apply(unset(servicePath("media")))
          }
        >
          <MediaServiceEditor config={config} />
        </Section>

        <Section
          title="File manager"
          description="Managing this device's files from the web interface (issue #195)."
          present={service.file_manager != null}
          onTogglePresent={(on) =>
            on
              ? apply(set(servicePath("file_manager"), { enabled: true } as never))
              : apply(unset(servicePath("file_manager")))
          }
        >
          <FileManagerEditor config={config} />
        </Section>

        <Section
          title="Steam"
          description="Behaviour of activities launched through the Steam snap."
          present={service.steam != null}
          onTogglePresent={(on) =>
            on
              ? apply(
                  set(servicePath("steam"), {
                    allow_risky_dismiss: false,
                  } as never),
                )
              : apply(unset(servicePath("steam")))
          }
        >
          <SteamEditor config={config} />
        </Section>

        <Section
          title="HUD"
          description="Which edge of the screen the always-visible bar sits on."
        >
          <HudEditor config={config} />
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

        <DangerZone
          warning={
            <>
              These settings decide whether the device has a web interface at
              all, and who can reach it — including this editor, when it is
              opened from the device itself. Turning the API off, moving it to
              an address this browser cannot reach, or dropping TLS on a
              non-loopback bind will end the session that saved the change, and
              a hardened device has no SSH to go back in with.
            </>
          }
        >
          <Section
            title="Management API"
            description="The HTTP interface the web management UI and this editor talk to."
            present={service.management_api != null}
            onTogglePresent={(on) =>
              on
                ? apply(
                    set(servicePath("management_api"), {
                      enabled: true,
                    } as never),
                  )
                : apply(unset(servicePath("management_api")))
            }
          >
            <ManagementApiEditor config={config} />
          </Section>
        </DangerZone>

        <Section
          title="Bluetooth management"
          description="The companion app's transport; works without any network setup."
          present={service.ble_management != null}
          onTogglePresent={(on) =>
            on
              ? apply(
                  set(servicePath("ble_management"), {
                    enabled: true,
                  } as never),
                )
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
          placeholder={String(LOAD_TIME_DEFAULTS.internet_check_interval_seconds)}
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
          placeholder={String(LOAD_TIME_DEFAULTS.internet_check_timeout_ms)}
          onChange={(e) =>
            f.setField(
              "timeout_ms",
              e.target.value === "" ? undefined : Number(e.target.value),
            )
          }
        />
      </Stack>
    </Stack>
  );
}

/** One GiB, the unit both size fields are actually configured in. */
const GIB = 1024 ** 3;

/**
 * Sizes are stored as bytes but edited in GiB — `cache_max_bytes = 10737418240`
 * is not a number anyone types correctly. The field shows whatever is stored,
 * fractional if it is not a whole GiB, and only writes when the value changes,
 * so opening the page never rewrites the file.
 */
function GibField({
  label,
  value,
  onChange,
  helperText,
}: {
  label: string;
  value: number | undefined;
  onChange: (bytes: number | undefined) => void;
  helperText?: string;
}) {
  return (
    <TextField
      size="small"
      type="number"
      label={label}
      value={value === undefined ? "" : value / GIB}
      onChange={(e) =>
        onChange(
          e.target.value === ""
            ? undefined
            : Math.round(Number(e.target.value) * GIB),
        )
      }
      slotProps={{ htmlInput: { step: 0.5, min: 0 } }}
      helperText={helperText}
    />
  );
}

/// Keyed by the generated union, so a category added to the config schema and
/// not here fails the build rather than quietly missing from the picker.
const SPONSORBLOCK_CATEGORIES: Record<RawSponsorBlockCategory, string> = {
  sponsor: "Sponsor",
  selfpromo: "Self-promotion",
  interaction: "\u201cLike and subscribe\u201d",
  intro: "Intro",
  outro: "End cards",
  preview: "Recap of an earlier episode",
  filler: "Filler tangent",
  music_offtopic: "Non-music section",
  hook: "Opening hook",
};

/**
 * A duration default, spelled the way the field's own control would show it.
 *
 * The value comes from `LOAD_TIME_DEFAULTS`, which carries seconds because
 * that is what the config key holds; a placeholder reading "120" beside a
 * control that accepts "2m" would be its own small lie.
 */
function secondsPlaceholder(seconds: number): string {
  if (seconds === 0) return "0";
  if (seconds % 3600 === 0) return `${seconds / 3600}h`;
  if (seconds % 60 === 0) return `${seconds / 60}m`;
  return `${seconds}s`;
}

function SponsorBlockEditor({ config }: { config: RawConfig }) {
  const f = useFields("service.media.sponsorblock");
  const v = config.service?.media?.sponsorblock;
  const enabled = v?.enabled ?? FIELD_DEFAULTS.RawSponsorBlockConfig.enabled;
  // Ticked while the config names none, so the boxes match what would happen.
  const categories: readonly RawSponsorBlockCategory[] =
    v?.categories ?? FIELD_DEFAULTS.RawSponsorBlockConfig.categories;

  const toggle = (category: RawSponsorBlockCategory, on: boolean) => {
    const next = (
      Object.keys(SPONSORBLOCK_CATEGORIES) as RawSponsorBlockCategory[]
    ).filter((c) => (c === category ? on : categories.includes(c)));
    f.setField("categories", next);
  };

  return (
    <Stack spacing={2} sx={{ maxWidth: 520 }}>
      <FormControlLabel
        control={
          <Switch
            checked={enabled}
            onChange={(e) => f.setField("enabled", e.target.checked)}
          />
        }
        label="Skip sponsored spans in YouTube videos"
      />
      <Typography variant="caption" color="text.secondary" sx={{ mt: -1 }}>
        Off by default. With it off nothing is looked up and nothing reaches
        sponsor.ajay.app — not at launch, not on a play, not in the background.
        Segment data is CC BY-NC-SA 4.0 from SponsorBlock.
      </Typography>
      {enabled && (
        <>
          <FormGroup>
            {(
              Object.keys(SPONSORBLOCK_CATEGORIES) as RawSponsorBlockCategory[]
            ).map((c) => (
              <FormControlLabel
                key={c}
                control={
                  <Checkbox
                    size="small"
                    checked={categories.includes(c)}
                    onChange={(e) => toggle(c, e.target.checked)}
                  />
                }
                label={SPONSORBLOCK_CATEGORIES[c]}
              />
            ))}
          </FormGroup>
          <Typography variant="caption" color="text.secondary" sx={{ mt: -1 }}>
            The last four are off by default: their submissions are judgement
            calls that can cut something a child wanted — a recap is part of the
            episode for a viewer who missed last week.
          </Typography>
          <TextField
            size="small"
            label="SponsorBlock instance"
            value={v?.api ?? ""}
            onChange={(e) => f.setField("api", e.target.value)}
            placeholder="https://sponsor.ajay.app"
            helperText="Point this at a mirror to keep lookups inside your own infrastructure."
          />
        </>
      )}
    </Stack>
  );
}

function MediaServiceEditor({ config }: { config: RawConfig }) {
  const f = useFields("service.media");
  const v = config.service?.media;
  return (
    <Stack spacing={2} sx={{ maxWidth: 520 }}>
      <FormControlLabel
        control={
          <Switch
            checked={v?.prefetch ?? FIELD_DEFAULTS.RawMediaServiceConfig.prefetch}
            onChange={(e) => f.setField("prefetch", e.target.checked)}
          />
        }
        label="Download remote items in the background"
      />
      <Typography variant="caption" color="text.secondary" sx={{ mt: -1 }}>
        So a video opened later plays from disk instead of buffering. Individual
        activities can opt out on their own.
      </Typography>
      <FormControlLabel
        control={
          <Switch
            checked={
              v?.prefetch_while_session_active ??
              FIELD_DEFAULTS.RawMediaServiceConfig.prefetch_while_session_active
            }
            onChange={(e) =>
              f.setField("prefetch_while_session_active", e.target.checked)
            }
          />
        }
        label="Keep downloading while an activity is running"
      />
      <Typography variant="caption" color="text.secondary" sx={{ mt: -1 }}>
        Off by default: a download competing with a game — or with the video
        being watched right now — costs CPU and bandwidth for content nobody has
        asked for yet.
      </Typography>
      <Stack direction="row" spacing={2}>
        <GibField
          label="Cache limit (GiB)"
          value={v?.cache_max_bytes}
          onChange={(bytes) => f.setField("cache_max_bytes", bytes)}
          helperText="Total size of the video cache."
        />
        <GibField
          label="Keep free (GiB)"
          value={v?.free_space_floor_bytes}
          onChange={(bytes) => f.setField("free_space_floor_bytes", bytes)}
          helperText="Stop and warn below this. 0 disables."
        />
      </Stack>
      <Typography variant="caption" color="text.secondary" sx={{ mt: -1 }}>
        Both are needed: a 10 GiB cache on a 16 GiB device fills the disk long
        before it fills the cache.
      </Typography>
      <TextField
        size="small"
        type="number"
        label="Watched grace (days)"
        value={v?.watched_grace_days ?? ""}
        placeholder={String(FIELD_DEFAULTS.RawMediaServiceConfig.watched_grace_days)}
        onChange={(e) =>
          f.setField(
            "watched_grace_days",
            e.target.value === "" ? undefined : Number(e.target.value),
          )
        }
        slotProps={{ htmlInput: { min: 0 } }}
        helperText="How long watching a video protects its copy from being displaced. 0 orders purely by age."
      />
      <SponsorBlockEditor config={config} />
    </Stack>
  );
}

/**
 * `[service.file_manager]` (issue #195).
 *
 * `extra_roots` is a list of `{ label, path }` in the schema and a label →
 * path map here: the same shape, and `KeyValueEditor` already knows how to
 * render one without a second repeated-row editor existing. Two roots sharing
 * a label would collapse into one — which config validation refuses anyway,
 * for the same reason it would be confusing on screen.
 */
function FileManagerEditor({ config }: { config: RawConfig }) {
  const f = useFields("service.file_manager");
  const v = config.service?.file_manager;
  const roots: Record<string, string> = Object.fromEntries(
    (v?.extra_roots ?? []).map((r) => [r.label, r.path]),
  );
  return (
    <Stack spacing={2} sx={{ maxWidth: 520 }}>
      <FormControlLabel
        control={
          <Switch
            checked={v?.enabled ?? FIELD_DEFAULTS.RawFileManagerConfig.enabled}
            onChange={(e) => f.setField("enabled", e.target.checked)}
          />
        }
        label="Manage this device's files from the web interface"
      />
      <Typography variant="caption" color="text.secondary" sx={{ mt: -1 }}>
        Rooted at this kiosk user's home directory, which SSH and SFTP cannot
        reach on a hardened account. Off means the routes are not served at
        all; there is deliberately no on/off switch elsewhere in the interface.
      </Typography>
      <Stack direction="row" spacing={2}>
        <GibField
          label="Largest upload (GiB)"
          value={v?.max_upload_bytes}
          onChange={(bytes) => f.setField("max_upload_bytes", bytes)}
          helperText="0 removes the cap."
        />
        <GibField
          label="Keep free (GiB)"
          value={v?.free_space_floor_bytes}
          onChange={(bytes) => f.setField("free_space_floor_bytes", bytes)}
          helperText="Refuse an upload that would cross this. 0 disables."
        />
      </Stack>
      <FormControlLabel
        control={
          <Switch
            checked={
              v?.external_media ?? FIELD_DEFAULTS.RawFileManagerConfig.external_media
            }
            onChange={(e) => f.setField("external_media", e.target.checked)}
          />
        }
        label="Offer removable drives"
      />
      <Typography variant="caption" color="text.secondary" sx={{ mt: -1 }}>
        Anything mounted under /media or /run/media, identified by its
        filesystem UUID so a drive keeps its place when it is plugged in again.
      </Typography>
      <KeyValueEditor
        label="Extra places to browse"
        values={roots}
        onChange={(next) =>
          f.setField(
            "extra_roots",
            Object.entries(next).map(([label, path]) => ({ label, path })),
          )
        }
      />
      <Typography variant="caption" color="text.secondary" sx={{ mt: -1 }}>
        A name, and an absolute path — a NAS mount, or a library on a second
        disk. `/`, `/etc` and the other system directories are refused.
      </Typography>
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
            checked={v?.allow_risky_dismiss ?? FIELD_DEFAULTS.RawSteamConfig.allow_risky_dismiss}
            onChange={(e) =>
              f.setField("allow_risky_dismiss", e.target.checked)
            }
          />
        }
        label="Allow risky interstitial kinds"
      />
      <Alert severity="warning">
        Risky kinds dismiss modals whose game cannot actually be played without
        missing hardware — the activity launches into something unusable.
      </Alert>
      <TextField
        size="small"
        type="number"
        label="Launch timeout (s)"
        value={v?.launch_timeout_seconds ?? ""}
        placeholder={String(LOAD_TIME_DEFAULTS.steam_launch_timeout_seconds)}
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

function HudEditor({ config }: { config: RawConfig }) {
  const { apply } = useConfigDoc();
  const f = useFields("service.hud");
  const v = config.service?.hud;
  return (
    <Stack spacing={1} sx={{ maxWidth: 520 }}>
      <TextField
        select
        size="small"
        label="HUD edge"
        // Unset has to *read* as the default edge, not as a blank box: which
        // edge the
        // device uses when nothing says otherwise is the whole question this
        // control answers, and a blank field leaves it unanswered. MUI renders
        // an empty value as nothing unless told otherwise, and the label then
        // needs pinning up so it does not sit on top of the text.
        slotProps={{
          select: { displayEmpty: true },
          inputLabel: { shrink: true },
        }}
        value={v?.orientation ?? ""}
        onChange={(e) =>
          // Clearing removes the whole `[service.hud]` table rather than
          // leaving an empty one behind: the daemon falls back to
          // `LOAD_TIME_DEFAULTS.hud_orientation` either way, and a table with
          // nothing in it is noise in a file people hand-annotate.
          e.target.value
            ? f.setField("orientation", e.target.value)
            : apply(unset(servicePath("hud")))
        }
        helperText="Applies to the launcher and to every activity that does not choose its own."
        fullWidth
      >
        <MenuItem value="">{DEFAULT_HUD_ORIENTATION_LABEL} (the default)</MenuItem>
        {HUD_ORIENTATIONS.map((o) => (
          <MenuItem key={o.value} value={o.value}>
            {o.label}
          </MenuItem>
        ))}
      </TextField>
      <Typography variant="caption" color="text.secondary">
        {VERTICAL_HUD_DESCRIPTION} An individual activity can override this on
        its Behaviour tab, and the HUD moves back here when that activity ends.
      </Typography>
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
            checked={v?.docking_enabled ?? FIELD_DEFAULTS.RawDisplayConfig.docking_enabled}
            onChange={(e) => f.setField("docking_enabled", e.target.checked)}
          />
        }
        label="Manage external displays"
      />
      <FormControlLabel
        control={
          <Switch
            checked={v?.mirror_audio ?? FIELD_DEFAULTS.RawDisplayConfig.mirror_audio}
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
            checked={v?.enabled ?? FIELD_DEFAULTS.RawManagementApiConfig.enabled}
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
          placeholder={String(LOAD_TIME_DEFAULTS.management_api_port)}
          onChange={(e) =>
            f.setField(
              "port",
              e.target.value === "" ? undefined : Number(e.target.value),
            )
          }
        />
        <TextField
          size="small"
          label="Bind address"
          value={v?.bind ?? ""}
          placeholder={LOAD_TIME_DEFAULTS.management_api_bind}
          onChange={(e) => f.setField("bind", e.target.value)}
          sx={{ flex: 1 }}
        />
      </Stack>
      <TextField
        size="small"
        label="Machine token"
        value={v?.auth_token ?? ""}
        onChange={(e) => f.setField("auth_token", e.target.value)}
        helperText="For scripts and the e2e harness. It authenticates a request
          but cannot sign anybody in — a person uses a password or the paired
          companion. Leave empty unless something automated needs it."
      />
      <TlsEditor config={config} />
      <SessionEditor config={config} />
      <TextField
        size="small"
        type="number"
        label="Bind retry (s)"
        value={v?.bind_retry_seconds ?? ""}
        placeholder={String(LOAD_TIME_DEFAULTS.management_api_bind_retry_seconds)}
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

/**
 * Transport security for the management API (issue #156).
 *
 * The mode a config that says nothing gets is `auto`, and it is the right one
 * almost always: plaintext on loopback, a generated self-signed certificate
 * anywhere else. The reason to touch this is `files` — a certificate from
 * `tailscale cert`, Let's Encrypt or a home CA, which is the only way to get a
 * browser padlock with no warning.
 */
function TlsEditor({ config }: { config: RawConfig }) {
  const f = useFields("service.management_api.tls");
  const v = config.service?.management_api?.tls;
  const mode = v?.mode ?? "auto";
  const bind = config.service?.management_api?.bind;
  return (
    <Stack spacing={2}>
      <TextField
        size="small"
        select
        label="TLS"
        value={mode}
        onChange={(e) =>
          f.setField("mode", e.target.value === "auto" ? undefined : e.target.value)
        }
        helperText="Auto: plaintext on loopback, a self-signed certificate on any
          other address."
        sx={{ maxWidth: 320 }}
      >
        <MenuItem value="auto">Automatic</MenuItem>
        <MenuItem value="off">Off (plaintext)</MenuItem>
        <MenuItem value="self_signed">Self-signed certificate</MenuItem>
        <MenuItem value="files">Certificate files</MenuItem>
      </TextField>
      {mode === "files" && (
        <>
          <TextField
            size="small"
            label="Certificate (PEM)"
            value={v?.cert ?? ""}
            onChange={(e) => f.setField("cert", e.target.value)}
            placeholder="/var/lib/shepherdd/tls/fullchain.pem"
          />
          <TextField
            size="small"
            label="Private key (PEM)"
            value={v?.key ?? ""}
            onChange={(e) => f.setField("key", e.target.value)}
            placeholder="/var/lib/shepherdd/tls/privkey.pem"
          />
        </>
      )}
      {mode === "off" && bind && bind !== "127.0.0.1" && bind !== "::1" && (
        <Alert severity="error">
          Plaintext on {bind} serves administration in the clear to everyone on
          that network, including the child this device manages. The daemon
          refuses to start with this combination.
        </Alert>
      )}
    </Stack>
  );
}

/** How long a signed-in browser stays signed in, and what a guesser costs. */
function SessionEditor({ config }: { config: RawConfig }) {
  const f = useFields("service.management_api.auth");
  const v = config.service?.management_api?.auth;
  const numberField = (
    name: "session_idle_days" | "session_max_days" | "lockout_after" | "lockout_seconds",
    label: string,
    placeholder: string,
    helperText?: string,
  ) => (
    <TextField
      size="small"
      type="number"
      label={label}
      value={v?.[name] ?? ""}
      placeholder={placeholder}
      helperText={helperText}
      onChange={(e) =>
        f.setField(name, e.target.value === "" ? undefined : Number(e.target.value))
      }
    />
  );
  return (
    <Stack spacing={2}>
      <Typography variant="body2" color="text.secondary">
        Sign-in sessions
      </Typography>
      <Stack direction="row" spacing={2}>
        {numberField("session_idle_days", "Idle timeout (days)", "2")}
        {numberField("session_max_days", "Maximum age (days)", "14")}
      </Stack>
      <Stack direction="row" spacing={2}>
        {numberField("lockout_after", "Lock out after", "8", "failed attempts")}
        {numberField("lockout_seconds", "Lockout (s)", "300", "doubles each time")}
      </Stack>
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
            checked={v?.enabled ?? FIELD_DEFAULTS.RawBleManagementConfig.enabled}
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
            checked={
              s.capture_child_output ??
              FIELD_DEFAULTS.RawServiceConfig.capture_child_output
            }
            onChange={(e) =>
              f.setField("capture_child_output", e.target.checked)
            }
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
