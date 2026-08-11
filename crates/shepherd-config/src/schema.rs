//! Raw configuration schema (as parsed from TOML)

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Raw configuration as parsed from TOML
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RawConfig {
    /// Config schema version
    pub config_version: u32,

    /// Global service settings
    #[serde(default, alias = "daemon")]
    pub service: RawServiceConfig,

    /// List of allowed entries
    #[serde(default)]
    pub entries: Vec<RawEntry>,

    /// Groups of entries sharing a schedule and limits (issue #5)
    #[serde(default)]
    pub groups: Vec<RawGroup>,
}

/// A group of entries that share an availability schedule and a set of limits
/// (issue #5).
///
/// The daily quota is the *combined* usage of every member, so once the
/// category's budget is spent all of its activities disappear at once.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RawGroup {
    /// Unique stable ID, referenced by `group = "..."` on entries
    pub id: String,

    /// Display label, used when explaining why a member is unavailable
    pub label: String,

    /// Availability windows shared by every member
    #[serde(default)]
    pub availability: Option<RawAvailability>,

    /// Limits shared by every member. `daily_quota_seconds` is the combined
    /// total across members; `max_run_seconds` and `cooldown_seconds` apply to
    /// each member's session.
    #[serde(default)]
    pub limits: Option<RawLimits>,

    /// Token gate on the whole group: earning unlocks every member at once
    #[serde(default)]
    pub tokens: Option<RawTokens>,
}

/// Service-level settings
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawServiceConfig {
    /// IPC socket path (default: $XDG_RUNTIME_DIR/shepherdd/shepherdd.sock)
    pub socket_path: Option<PathBuf>,

    /// Log directory (default: $XDG_STATE_HOME/shepherdd)
    pub log_dir: Option<PathBuf>,

    /// Data directory for store (default: $XDG_DATA_HOME/shepherdd)
    pub data_dir: Option<PathBuf>,

    /// Capture stdout/stderr from child applications to log files
    /// Files are written to child_log_dir (or log_dir/sessions if not set)
    #[serde(default)]
    pub capture_child_output: bool,

    /// Directory for child application logs (default: log_dir/sessions)
    pub child_log_dir: Option<PathBuf>,

    /// Default warning thresholds (can be overridden per entry)
    pub default_warnings: Option<Vec<RawWarningThreshold>>,

    /// Default max run duration
    pub default_max_run_seconds: Option<u64>,

    /// Minimum session length before a cooldown is started, in seconds
    /// (default 120). A session shorter than this leaves the cooldown alone,
    /// so an activity that crashes seconds after launch doesn't lock the child
    /// out. Set to 0 to always start the cooldown; overridable per entry and
    /// per group via `limits.cooldown_min_session_seconds`.
    pub cooldown_min_session_seconds: Option<u64>,

    /// Global volume restrictions
    #[serde(default)]
    pub volume: Option<RawVolumeConfig>,

    /// Global screen-brightness restrictions
    #[serde(default)]
    pub brightness: Option<RawBrightnessConfig>,

    /// Internet connectivity check settings
    #[serde(default)]
    pub internet: Option<RawInternetConfig>,

    /// Steam-specific behaviour
    #[serde(default)]
    pub steam: Option<RawSteamConfig>,

    /// Management HTTP API settings
    #[serde(default)]
    pub management_api: Option<RawManagementApiConfig>,

    /// Bluetooth LE management transport. Designed as the primary admin
    /// path (works without IP autodiscovery or static IP). See
    /// `docs/ai/history/2026-06-20 002 ble-management.md`.
    pub ble_management: Option<RawBleManagementConfig>,

    /// External monitor / docking behaviour (issue #87).
    #[serde(default)]
    pub display: Option<RawDisplayConfig>,
}

/// Raw entry definition
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RawEntry {
    /// Unique stable ID
    pub id: String,

    /// Display label
    pub label: String,

    /// Icon reference (opaque, interpreted by shell)
    pub icon: Option<String>,

    /// Entry kind and launch details
    pub kind: RawEntryKind,

    /// Availability time windows
    #[serde(default)]
    pub availability: Option<RawAvailability>,

    /// Time limits
    #[serde(default)]
    pub limits: Option<RawLimits>,

    /// Token gate (issue #8): time banked by other activities unlocks this one
    #[serde(default)]
    pub tokens: Option<RawTokens>,

    /// Group this entry belongs to (issue #5). The group's schedule and limits
    /// apply on top of this entry's own; the strictest of each wins.
    #[serde(default)]
    pub group: Option<String>,

    /// Warning configuration
    #[serde(default)]
    pub warnings: Option<Vec<RawWarningThreshold>>,

    /// Volume restrictions for this entry (overrides global)
    #[serde(default)]
    pub volume: Option<RawVolumeConfig>,

    /// Screen-brightness restrictions for this entry (overrides global)
    #[serde(default)]
    pub brightness: Option<RawBrightnessConfig>,

    /// Explicitly disabled
    #[serde(default)]
    pub disabled: bool,

    /// Reason for disabling
    pub disabled_reason: Option<String>,

    /// Internet requirement for this entry
    #[serde(default)]
    pub internet: Option<RawEntryInternet>,

    /// Network firewall rules applied while this entry is running
    #[serde(default)]
    pub firewall: Option<RawFirewallConfig>,

    /// Supervised-browser policy (Chromium enterprise policy + profile).
    /// Compose with `kind = { type = "flatpak", app_id = "com.google.Chrome" }`
    /// and an optional `[entries.firewall]` to build the web-browser activity.
    #[serde(default)]
    pub browser: Option<RawBrowserConfig>,

    /// Input compatibility modes for this entry. Each mode runs an
    /// orthogonal sidecar — touch-to-mouse and gamepad presets can be
    /// stacked. Accepts a single string (`input_compat = "touch_to_mouse"`)
    /// or a list (`input_compat = ["touch_to_mouse", "gamepad_productivity"]`).
    #[serde(default, deserialize_with = "deserialize_input_compat_list")]
    pub input_compat: Vec<RawInputCompat>,

    /// Tunables for input-compat sidecars (analog deadzones, speeds).
    #[serde(default)]
    pub input_compat_options: Option<RawInputCompatOptions>,

    /// Physical input devices this entry depends on (issue #96). The entry is
    /// only shown / launchable while every listed device type is connected;
    /// e.g. `requires_input = "keyboard"` hides a typing tutor until a keyboard
    /// is attached. Accepts a single string (`requires_input = "keyboard"`) or
    /// a list (`requires_input = ["keyboard", "mouse"]`). Empty / absent means
    /// no input requirement.
    #[serde(default, deserialize_with = "deserialize_input_device_list")]
    pub requires_input: Vec<RawInputDevice>,

    /// Drop the compositor output scale to 1.0 for the duration of this
    /// activity so XWayland clients render at the panel's native resolution.
    /// Sway doesn't pass scale through to XWayland (issue #45), so without
    /// this an XWayland game at `output * scale 1.5` only fills 1280x720 of
    /// a 1920x1080 panel. shepherdd compensates by telling the HUD to apply
    /// a counter-scale factor so it stays a normal size while the activity
    /// runs.
    #[serde(default)]
    pub xwayland_native_resolution: bool,

    /// Ask for confirmation before the HUD "X" (End session) button ends this
    /// activity. Since the button is easy to hit by accident and many
    /// activities lose unsaved state when force-closed, the HUD shows a
    /// confirmation prompt first (issue #78). Only affects the "X" button —
    /// closing via the API, time expiration, or the process exiting is
    /// unaffected. Enabled by default; set `false` for activities that are
    /// safe to close instantly.
    #[serde(default = "default_true")]
    pub confirm_on_close: bool,
}

/// Per-entry firewall configuration
///
/// Enforced via systemd `IPAddressAllow=`/`IPAddressDeny=` properties on the
/// per-session scope. Hostname matching is **not** performed at the kernel
/// layer; pair with a browser-side allowlist (e.g. Chrome `URLAllowlist`) when
/// hostname resolution is needed.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawFirewallConfig {
    /// Default policy when no `allow` or `deny` rule matches.
    /// "deny" (default) blocks all traffic except `allow` entries.
    /// "allow" permits all traffic except `deny` entries.
    #[serde(default = "default_firewall_default")]
    pub default: String,

    /// Allowlisted destinations (CIDR or systemd address tokens like "any",
    /// "localhost", "link-local", "multicast")
    #[serde(default)]
    pub allow: Vec<String>,

    /// Denylisted destinations (applied after `allow`)
    #[serde(default)]
    pub deny: Vec<String>,
}

fn default_firewall_default() -> String {
    "deny".to_string()
}

/// Per-entry supervised-browser policy.
///
/// Materialized at spawn time into a Chromium [managed-policy JSON][policies]
/// file plus a set of Chrome command-line flags. Hostname allowlisting is
/// enforced by the browser itself via `URLAllowlist`/`URLBlocklist` (no
/// extensions); pair with [`RawFirewallConfig`] for coarse IP-layer
/// defense-in-depth. shepherd-launcher only wraps Chrome through documented
/// controls — it does not patch the browser or circumvent any protections.
///
/// [policies]: https://chromeenterprise.google/policies/
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawBrowserConfig {
    /// Filesystem segment selecting the on-disk user-data-dir. Entries that
    /// share a `profile_id` share cookies/logins; each unique id is isolated.
    /// Must be a single safe path segment (no separators, not `.`/`..`).
    pub profile_id: String,

    /// Window mode: "kiosk"/"app" both open a chromeless window (no tabs or
    /// omnibox) via Chrome's `--app`, or "windowed" (normal browser window).
    /// Default "kiosk". Note: shepherd's sway compositor denies clients true
    /// fullscreen to keep the HUD visible, so "kiosk" does not use `--kiosk`
    /// (which would fall back to a toolbar'd window); it behaves like "app".
    #[serde(default = "default_browser_mode")]
    pub mode: String,

    /// URL opened on launch. Must be an http(s) URL when set.
    pub start_url: Option<String>,

    /// Chromium `URLAllowlist` patterns. Empty = no allowlist (all URLs
    /// permitted, subject to `url_blocklist`).
    #[serde(default)]
    pub url_allowlist: Vec<String>,

    /// Chromium `URLBlocklist` patterns, applied after the allowlist.
    #[serde(default)]
    pub url_blocklist: Vec<String>,

    /// Disable DevTools (`DeveloperToolsDisabled`). Default true.
    #[serde(default = "default_true")]
    pub disable_dev_tools: bool,

    /// Disable incognito mode (`IncognitoModeAvailability`). Default true.
    #[serde(default = "default_true")]
    pub disable_incognito: bool,

    /// Block extension installation (`ExtensionInstallBlocklist = ["*"]`).
    /// Default true.
    #[serde(default = "default_true")]
    pub disable_extensions: bool,

    /// Wipe the on-disk profile directory after the session ends (handled by
    /// the host adapter's post-exit cleanup, not by Chrome). Default false.
    #[serde(default)]
    pub wipe_on_exit: bool,
}

fn default_browser_mode() -> String {
    "kiosk".to_string()
}

/// Input compatibility mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RawInputCompat {
    /// Translate touchscreen input into mouse events via a sidecar that
    /// grabs touch devices and uses the Wayland virtual-pointer protocol.
    TouchToMouse,
    /// Translate absolute pointer / tablet input into touch events via a
    /// sidecar that grabs the device and emits a virtual touchscreen. The
    /// inverse of `TouchToMouse`; the two must not be combined.
    TabletToTouch,
    /// Grab every touchscreen and discard its events, disabling the
    /// touchscreen for the duration of the activity. Mutually exclusive with
    /// `TouchToMouse` and `TabletToTouch`.
    DisableTouch,
    /// Productivity preset: triggers = LMB, shoulders = RMB, left stick =
    /// mouse, right stick = scroll, stick-click toggles which stick drives
    /// the mouse, D-pad = arrow keys, A = Enter, Start = Escape.
    GamepadProductivity,
    /// GPD/FPS preset: LT = LMB, RT = RMB, LB = MMB, left stick = WASD,
    /// right stick = mouse, D-pad = scroll, A = Space, X = R, B = E, Y = F.
    GamepadGpd,
}

/// Per-entry tunables forwarded to input-compat sidecars.
#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize, Serialize)]
pub struct RawInputCompatOptions {
    /// Stick deadzone as a fraction of full deflection (0..1).
    pub gamepad_deadzone: Option<f32>,
    /// Mouse speed in pixels per second at full stick deflection.
    pub gamepad_mouse_speed: Option<f32>,
    /// Scroll speed in discrete wheel units per second at full deflection.
    pub gamepad_scroll_speed: Option<f32>,
}

/// Accept either a single `RawInputCompat` value or a list of them. Empty
/// list and missing field both deserialize to `vec![]`.
fn deserialize_input_compat_list<'de, D>(deserializer: D) -> Result<Vec<RawInputCompat>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(RawInputCompat),
        Many(Vec<RawInputCompat>),
    }

    match Option::<OneOrMany>::deserialize(deserializer)? {
        None => Ok(Vec::new()),
        Some(OneOrMany::One(v)) => Ok(vec![v]),
        Some(OneOrMany::Many(v)) => Ok(v),
    }
}

/// A category of physical input device an activity can depend on (issue #96).
///
/// Unlike [`RawInputCompat`], which is a *spawn-time behaviour* (it launches an
/// input-translation sidecar), this is a *gating* condition: the activity is
/// only shown / launchable when every listed device type is connected. The
/// enum is closed, so `camera`, `microphone`, and `midi` (future work) fail to
/// parse rather than being silently accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RawInputDevice {
    /// A relative pointing device (mouse, trackball, trackpad).
    Mouse,
    /// A finger touchscreen.
    Touch,
    /// A physical alphabetic keyboard.
    Keyboard,
    /// A gamepad / game controller / joystick.
    Gamepad,
}

/// Accept either a single `RawInputDevice` value or a list of them. Empty list
/// and missing field both deserialize to `vec![]`.
fn deserialize_input_device_list<'de, D>(deserializer: D) -> Result<Vec<RawInputDevice>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(RawInputDevice),
        Many(Vec<RawInputDevice>),
    }

    match Option::<OneOrMany>::deserialize(deserializer)? {
        None => Ok(Vec::new()),
        Some(OneOrMany::One(v)) => Ok(vec![v]),
        Some(OneOrMany::Many(v)) => Ok(v),
    }
}

/// Raw entry kind
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RawEntryKind {
    Process {
        /// Command to run (required)
        command: String,
        /// Additional command-line arguments
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: HashMap<String, String>,
        cwd: Option<PathBuf>,
    },
    /// Snap application - uses systemd scope-based process management
    Snap {
        /// The snap name (e.g., "mc-installer")
        snap_name: String,
        /// Command to run (defaults to snap_name if not specified)
        command: Option<String>,
        /// Additional command-line arguments
        #[serde(default)]
        args: Vec<String>,
        /// Additional environment variables
        #[serde(default)]
        env: HashMap<String, String>,
    },
    /// Steam game launched via the Steam snap (Linux)
    Steam {
        /// Steam App ID (e.g., 504230 for Celeste)
        app_id: u32,
        /// Additional command-line arguments passed to Steam
        #[serde(default)]
        args: Vec<String>,
        /// Additional environment variables
        #[serde(default)]
        env: HashMap<String, String>,
    },
    /// Flatpak application - uses systemd scope-based process management
    Flatpak {
        /// The Flatpak application ID (e.g., "org.prismlauncher.PrismLauncher")
        app_id: String,
        /// Additional command-line arguments
        #[serde(default)]
        args: Vec<String>,
        /// Additional environment variables
        #[serde(default)]
        env: HashMap<String, String>,
    },
    Vm {
        driver: String,
        #[serde(default)]
        args: HashMap<String, serde_json::Value>,
    },
    Media {
        library_id: String,
        #[serde(default)]
        args: HashMap<String, serde_json::Value>,
    },
    Custom {
        type_name: String,
        #[serde(default)]
        payload: Option<serde_json::Value>,
    },
}

/// Availability configuration
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawAvailability {
    /// Time windows when entry is available
    #[serde(default)]
    pub windows: Vec<RawTimeWindow>,

    /// If true, entry is always available (ignores windows)
    #[serde(default)]
    pub always: bool,
}

/// Time window
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RawTimeWindow {
    /// Days of week: "weekdays", "weekends", "all", or list like ["mon", "tue", "wed"]
    pub days: RawDays,

    /// Start time (HH:MM format)
    pub start: String,

    /// End time (HH:MM format)
    pub end: String,
}

/// Days specification
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum RawDays {
    Preset(String),
    List(Vec<String>),
}

/// Time limits
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawLimits {
    /// Maximum run duration in seconds
    pub max_run_seconds: Option<u64>,

    /// Daily quota in seconds
    pub daily_quota_seconds: Option<u64>,

    /// Cooldown after session ends, in seconds
    pub cooldown_seconds: Option<u64>,

    /// Minimum session length before this subject's cooldown is started, in
    /// seconds. Overrides `service.cooldown_min_session_seconds` (default 120).
    /// 0 means the cooldown always starts, however short the session was.
    pub cooldown_min_session_seconds: Option<u64>,
}

/// Token gate (issue #8)
///
/// Configured on the *target* entry: time spent on the entries listed in
/// `from` banks a balance that this entry spends down as it runs.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawTokens {
    /// Subjects whose sessions bank time toward this one: an entry ID, or a
    /// group ID prefixed with `group:` to count every member of a category.
    #[serde(default)]
    pub from: Vec<String>,

    /// Seconds earned per second spent on a source entry. Default 1.0.
    pub earn_ratio: Option<f64>,

    /// Balance required before this entry unlocks at all. Default 0, meaning
    /// any balance above zero unlocks it.
    pub minimum_seconds: Option<u64>,

    /// Ceiling on the banked balance. 0 (the default) means unlimited.
    pub max_balance_seconds: Option<u64>,

    /// Whether the balance survives local midnight. Default false, matching
    /// how the daily quota resets.
    #[serde(default)]
    pub carry_over: bool,
}

/// Warning threshold
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RawWarningThreshold {
    /// Seconds before expiry
    pub seconds_before: u64,

    /// Severity: "info", "warn", "critical"
    #[serde(default = "default_severity")]
    pub severity: String,

    /// Message template
    pub message: Option<String>,
}

/// Internet connectivity check configuration
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawInternetConfig {
    /// Connectivity check target (e.g., "https://example.com" or "tcp://1.1.1.1:53")
    pub check: Option<String>,

    /// Interval between checks (seconds)
    pub interval_seconds: Option<u64>,

    /// Timeout per check (milliseconds)
    pub timeout_ms: Option<u64>,
}

/// Steam-specific service configuration
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawSteamConfig {
    /// Known Steam launch interstitials (blocking modals between launch and the
    /// game starting) to auto-dismiss by clicking their affirmative button, so
    /// they don't hang the kiosk on a modal it can't show. Each value is an
    /// interstitial slug (e.g. "cloud_sync", "controller_recommended"). Only
    /// listed kinds are dismissed; an empty list disables the feature entirely
    /// (and the CEF remote-debugging port is never opened). When unset, a safe
    /// default set of verified, benign kinds is used.
    pub auto_dismiss_interstitials: Option<Vec<String>>,

    /// Allow "risky" interstitial kinds (those whose dismissal launches a game
    /// that can't actually be used without missing hardware, e.g.
    /// "controller_required") to appear in `auto_dismiss_interstitials`. Without
    /// this, listing a risky kind is a configuration error.
    #[serde(default)]
    pub allow_risky_dismiss: bool,

    /// How long to wait for a Steam game window/process to appear after launch
    /// before giving up and ending the session with an error (seconds).
    pub launch_timeout_seconds: Option<u64>,
}

/// Per-entry internet requirement
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawEntryInternet {
    /// Whether this entry requires internet connectivity
    #[serde(default)]
    pub required: bool,

    /// Override connectivity check target for this entry
    pub check: Option<String>,
}

fn default_severity() -> String {
    "warn".to_string()
}

/// Management HTTP API configuration
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RawManagementApiConfig {
    /// Whether the management API is enabled (default: false)
    #[serde(default)]
    pub enabled: bool,

    /// TCP port to listen on (default: 7890)
    pub port: Option<u16>,

    /// IP address to bind to (default: "127.0.0.1")
    pub bind: Option<String>,

    /// How long (in seconds) to keep retrying the initial bind when the requested address is
    /// unavailable (e.g. an interface like ZeroTier that has not yet come up). 0 means retry
    /// indefinitely. Default: 300.
    pub bind_retry_seconds: Option<u64>,

    /// Optional Bearer token for authentication. If absent, all LAN clients are trusted.
    pub auth_token: Option<String>,
}

/// Bluetooth LE management transport configuration.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RawBleManagementConfig {
    /// Whether the BLE management transport is enabled (default: false).
    #[serde(default)]
    pub enabled: bool,

    /// Advertised local name and the device name returned in `DeviceInfo`.
    /// Defaults to `"shepherd"`. Pick something the companion app can
    /// disambiguate when multiple shepherd devices are in range.
    pub device_name: Option<String>,

    /// Where the admin record (`AdminRecord` TOML) is persisted.
    /// Defaults to `<data_dir>/admin.toml`.
    pub admin_record_path: Option<PathBuf>,

    /// Sentinel file path. When present at daemon startup, the admin
    /// record is wiped and the device returns to the unclaimed state
    /// (and the file is removed). Defaults to
    /// `<data_dir>/.factory-reset-ble`.
    pub reset_sentinel_path: Option<PathBuf>,

    /// Which Bluetooth controller to serve on, when the host has more
    /// than one.
    ///
    /// Accepts a controller address (`"DC:56:7B:1F:7D:EA"`, preferred)
    /// or an interface name (`"hci1"`). Defaults to whichever adapter
    /// BlueZ lists first, which is **not** stable: the index tracks
    /// probe order, so re-plugging a dongle, a rebind, or a boot that
    /// enumerates USB differently can silently move the daemon onto the
    /// other radio. The address is burned into the controller and is the
    /// only identifier BlueZ exposes that both distinguishes adapters
    /// and survives that.
    pub adapter: Option<String>,
}

/// Volume control configuration
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawVolumeConfig {
    /// Maximum volume percentage allowed (0-100)
    pub max_volume: Option<u8>,

    /// Minimum volume percentage allowed (0-100)
    pub min_volume: Option<u8>,

    /// Whether mute toggle is allowed (default: true)
    #[serde(default = "default_true")]
    pub allow_mute: bool,

    /// Whether volume changes are allowed at all (default: true)
    #[serde(default = "default_true")]
    pub allow_change: bool,
}

/// Screen-brightness control configuration
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawBrightnessConfig {
    /// Maximum brightness percentage allowed (0-100)
    pub max_brightness: Option<u8>,

    /// Minimum brightness percentage allowed (0-100).
    /// Use this to prevent the screen from being driven all the way to 0%
    /// (which most panels interpret as "off" — confusing for a child user).
    pub min_brightness: Option<u8>,

    /// Whether brightness changes are allowed at all (default: true)
    #[serde(default = "default_true")]
    pub allow_change: bool,

    /// Automatic (ambient-light) brightness. Only honored under
    /// `[service.brightness]`; a copy on a per-entry `[entries.brightness]`
    /// override is ignored, since auto brightness is a device-global mode.
    #[serde(default)]
    pub auto: Option<RawAutoBrightnessConfig>,
}

/// Automatic screen-brightness configuration (ambient-light driven).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawAutoBrightnessConfig {
    /// Whether automatic brightness starts enabled. This is only the default;
    /// the runtime state (toggled from the HUD or management API) is persisted
    /// and takes precedence once set.
    #[serde(default)]
    pub enabled: bool,

    /// Ambient light (lux) at or below which the screen sits at `min_percent`.
    pub dim_lux: Option<f32>,

    /// Ambient light (lux) at or above which the screen sits at `max_percent`.
    pub bright_lux: Option<f32>,

    /// Brightness percent at the dim end of the curve (0-100).
    pub min_percent: Option<u8>,

    /// Brightness percent at the bright end of the curve (0-100).
    pub max_percent: Option<u8>,

    /// How often to sample the light sensor, in seconds.
    pub poll_interval_seconds: Option<u64>,
}

/// External monitor / docking settings (issue #87).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RawDisplayConfig {
    /// Master switch for docking support. When false, shepherdd leaves display
    /// configuration entirely to sway (default: true).
    #[serde(default = "default_true")]
    pub docking_enabled: bool,

    /// Route audio to the external video device while a secondary display is in
    /// use, in both mirror and external-only modes (default: true).
    #[serde(default = "default_true")]
    pub mirror_audio: bool,
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_process_entry() {
        let toml_str = r#"
            config_version = 1

            [[entries]]
            id = "scummvm"
            label = "ScummVM"
            kind = { type = "process", command = "scummvm", args = ["-f"] }

            [entries.limits]
            max_run_seconds = 3600
        "#;

        let config: RawConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.entries.len(), 1);
        assert_eq!(config.entries[0].id, "scummvm");
    }

    #[test]
    fn parse_input_compat_scalar() {
        let toml_str = r#"
            config_version = 1

            [[entries]]
            id = "g"
            label = "G"
            kind = { type = "process", command = "/bin/g" }
            input_compat = "touch_to_mouse"
        "#;
        let config: RawConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.entries[0].input_compat,
            vec![RawInputCompat::TouchToMouse]
        );
    }

    #[test]
    fn parse_input_compat_disable_touch() {
        let toml_str = r#"
            config_version = 1

            [[entries]]
            id = "g"
            label = "G"
            kind = { type = "process", command = "/bin/g" }
            input_compat = "disable_touch"
        "#;
        let config: RawConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.entries[0].input_compat,
            vec![RawInputCompat::DisableTouch]
        );
    }

    #[test]
    fn confirm_on_close_defaults_true_and_parses_false() {
        // Absent -> enabled by default (issue #78).
        let default_toml = r#"
            config_version = 1

            [[entries]]
            id = "g"
            label = "G"
            kind = { type = "process", command = "/bin/g" }
        "#;
        let config: RawConfig = toml::from_str(default_toml).unwrap();
        assert!(config.entries[0].confirm_on_close);

        // Explicit opt-out.
        let opt_out_toml = r#"
            config_version = 1

            [[entries]]
            id = "g"
            label = "G"
            kind = { type = "process", command = "/bin/g" }
            confirm_on_close = false
        "#;
        let config: RawConfig = toml::from_str(opt_out_toml).unwrap();
        assert!(!config.entries[0].confirm_on_close);
    }

    #[test]
    fn parse_input_compat_list() {
        let toml_str = r#"
            config_version = 1

            [[entries]]
            id = "g"
            label = "G"
            kind = { type = "process", command = "/bin/g" }
            input_compat = ["touch_to_mouse", "gamepad_productivity"]

            [entries.input_compat_options]
            gamepad_deadzone = 0.2
            gamepad_mouse_speed = 600.0
        "#;
        let config: RawConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.entries[0].input_compat,
            vec![
                RawInputCompat::TouchToMouse,
                RawInputCompat::GamepadProductivity
            ]
        );
        let opts = config.entries[0].input_compat_options.unwrap();
        assert_eq!(opts.gamepad_deadzone, Some(0.2));
        assert_eq!(opts.gamepad_mouse_speed, Some(600.0));
        assert_eq!(opts.gamepad_scroll_speed, None);
    }

    #[test]
    fn parse_input_compat_absent() {
        let toml_str = r#"
            config_version = 1

            [[entries]]
            id = "g"
            label = "G"
            kind = { type = "process", command = "/bin/g" }
        "#;
        let config: RawConfig = toml::from_str(toml_str).unwrap();
        assert!(config.entries[0].input_compat.is_empty());
    }

    #[test]
    fn parse_requires_input_scalar() {
        let toml_str = r#"
            config_version = 1

            [[entries]]
            id = "typing"
            label = "Typing Tutor"
            kind = { type = "process", command = "/bin/typing" }
            requires_input = "keyboard"
        "#;
        let config: RawConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.entries[0].requires_input,
            vec![RawInputDevice::Keyboard]
        );
    }

    #[test]
    fn parse_requires_input_list() {
        let toml_str = r#"
            config_version = 1

            [[entries]]
            id = "typing"
            label = "Typing Tutor"
            kind = { type = "process", command = "/bin/typing" }
            requires_input = ["keyboard", "mouse"]
        "#;
        let config: RawConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.entries[0].requires_input,
            vec![RawInputDevice::Keyboard, RawInputDevice::Mouse]
        );
    }

    #[test]
    fn parse_requires_input_absent() {
        let toml_str = r#"
            config_version = 1

            [[entries]]
            id = "g"
            label = "G"
            kind = { type = "process", command = "/bin/g" }
        "#;
        let config: RawConfig = toml::from_str(toml_str).unwrap();
        assert!(config.entries[0].requires_input.is_empty());
    }

    #[test]
    fn parse_requires_input_rejects_future_types() {
        // camera/microphone/midi are future work; the closed enum should make
        // configuring one a parse error rather than a silent no-op.
        let toml_str = r#"
            config_version = 1

            [[entries]]
            id = "g"
            label = "G"
            kind = { type = "process", command = "/bin/g" }
            requires_input = "camera"
        "#;
        assert!(toml::from_str::<RawConfig>(toml_str).is_err());
    }

    #[test]
    fn parse_browser_entry() {
        let toml_str = r#"
            config_version = 1

            [[entries]]
            id = "chrome-school"
            label = "School"
            kind = { type = "flatpak", app_id = "com.google.Chrome" }

            [entries.browser]
            profile_id = "school"
            mode = "kiosk"
            start_url = "https://classroom.google.com"
            url_allowlist = ["https://*.google.com/*"]
        "#;

        let config: RawConfig = toml::from_str(toml_str).unwrap();
        let browser = config.entries[0].browser.as_ref().unwrap();
        assert_eq!(browser.profile_id, "school");
        assert_eq!(browser.mode, "kiosk");
        assert_eq!(
            browser.start_url.as_deref(),
            Some("https://classroom.google.com")
        );
        assert_eq!(browser.url_allowlist, vec!["https://*.google.com/*"]);
        // Lockdown defaults are on; wipe defaults off.
        assert!(browser.disable_dev_tools);
        assert!(browser.disable_incognito);
        assert!(browser.disable_extensions);
        assert!(!browser.wipe_on_exit);
    }

    #[test]
    fn parse_time_windows() {
        let toml_str = r#"
            config_version = 1

            [[entries]]
            id = "game"
            label = "Game"
            kind = { type = "process", command = "/bin/game" }

            [entries.availability]
            [[entries.availability.windows]]
            days = "weekdays"
            start = "14:00"
            end = "18:00"

            [[entries.availability.windows]]
            days = ["sat", "sun"]
            start = "10:00"
            end = "20:00"
        "#;

        let config: RawConfig = toml::from_str(toml_str).unwrap();
        let avail = config.entries[0].availability.as_ref().unwrap();
        assert_eq!(avail.windows.len(), 2);
    }
}
