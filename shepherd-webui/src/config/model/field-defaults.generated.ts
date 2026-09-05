// GENERATED FILE — DO NOT EDIT BY HAND
//
// Rendered from `crates/shepherd-config/src/schema.rs` (serde defaults, via the
// JSON Schema) and `crates/shepherd-config/src/load_defaults.rs` (the ones
// resolved at policy load) by
// `cargo run -p shepherd-wire-codegen --bin rpc-codegen`.
// Edit the Rust and re-run instead.
//
// What a field falls back to when the config leaves it out. The editor needs
// these to render an unset control as the value the daemon will actually pick:
// an unset switch as on or off, an empty number field with its fallback as the
// placeholder, a slider with the inherited value drawn on the track.
//
// Not to be confused with `kind-defaults.generated.ts`, which answers what an
// entry's *kind* supplies for a field on the entry itself.

/**
 * Serde defaults, by the type that declares them.
 *
 * A field absent here has no default: it is either required, or an
 * `Option` the daemon resolves at load time (see `LOAD_TIME_DEFAULTS`).
 */
export const FIELD_DEFAULTS = {
  RawAutoBrightnessConfig: {
    enabled: false,
  },
  RawAvailability: {
    always: false,
    windows: [],
  },
  RawBleManagementConfig: {
    enabled: false,
  },
  RawBrightnessConfig: {
    allow_change: true,
  },
  RawBrowserConfig: {
    disable_dev_tools: true,
    disable_extensions: true,
    disable_incognito: true,
    mode: "kiosk",
    url_allowlist: [],
    url_blocklist: [],
    wipe_on_exit: false,
  },
  RawConfig: {
    entries: [],
    groups: [],
  },
  RawDisplayConfig: {
    docking_enabled: true,
    mirror_audio: true,
  },
  RawEntry: {
    disabled: false,
    requires_input: [],
    xwayland_native_resolution: false,
  },
  RawEntryInternet: {
    forward_check: true,
    required: false,
  },
  RawFirewallConfig: {
    allow: [],
    default: "deny",
    deny: [],
  },
  RawManagementApiConfig: {
    enabled: false,
  },
  RawMediaServiceConfig: {
    cache_max_bytes: 10737418240,
    free_space_floor_bytes: 2147483648,
    prefetch: true,
    prefetch_while_session_active: false,
    watched_grace_days: 30,
  },
  RawServiceConfig: {
    capture_child_output: false,
  },
  RawSponsorBlockConfig: {
    api: "https://sponsor.ajay.app",
    categories: ["sponsor", "selfpromo", "interaction", "intro", "outro"],
    enabled: false,
  },
  RawSteamConfig: {
    allow_risky_dismiss: false,
  },
  RawTokens: {
    carry_over: false,
    from: [],
  },
  RawVolumeConfig: {
    allow_change: true,
    allow_mute: true,
  },
  RawWarningThreshold: {
    severity: "warn",
  },
} as const;

/**
 * Serde defaults for fields inside an entry's `kind` table,
 * keyed by the `kind.type` they belong to.
 *
 * A kind with nothing to default is absent rather than empty, so a
 * lookup has to cope with `undefined` — `KIND_FIELD_DEFAULTS[k]?.x`.
 */
export const KIND_FIELD_DEFAULTS = {
  ebook: {
    args: [],
    env: {},
    font_family: "Noto Serif",
    font_size: 16,
    kiosk: true,
    layout: "facing_first_centered",
    viewer: "okular",
  },
  flatpak: {
    args: [],
    env: {},
  },
  media: {
    mode: "browse",
    quality: "1080p",
    resume: false,
    reverse: false,
    sort_by: "library",
  },
  process: {
    args: [],
    env: {},
  },
  retroarch: {
    args: [],
    command: "retroarch",
    env: {},
    kiosk: true,
    reset: true,
    save_state: "auto",
  },
  snap: {
    args: [],
    env: {},
  },
  steam: {
    args: [],
    env: {},
  },
  vm: {
    args: {},
  },
} as const;

/**
 * Defaults the daemon resolves at policy load rather than at
 * deserialization, so they are absent from the schema. Units and key
 * names are the config's own.
 */
export const LOAD_TIME_DEFAULTS = {
  cooldown_min_session_seconds: 120,
  hud_orientation: "top",
  internet_check_interval_seconds: 10,
  internet_check_timeout_ms: 1500,
  management_api_bind: "127.0.0.1",
  management_api_bind_retry_seconds: 300,
  management_api_port: 7890,
  max_run_seconds: 3600,
  save_grace_seconds: 120,
  steam_launch_timeout_seconds: 30,
  token_earn_ratio: 1.0,
  warnings: [{ seconds_before: 300, severity: "info" }, { seconds_before: 60, severity: "warn" }, { seconds_before: 10, severity: "critical" }],
} as const;
