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

    /// Global volume restrictions
    #[serde(default)]
    pub volume: Option<RawVolumeConfig>,

    /// Global screen-brightness restrictions
    #[serde(default)]
    pub brightness: Option<RawBrightnessConfig>,

    /// Internet connectivity check settings
    #[serde(default)]
    pub internet: Option<RawInternetConfig>,

    /// Management HTTP API settings
    #[serde(default)]
    pub management_api: Option<RawManagementApiConfig>,
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

    /// Input compatibility modes for this entry. Each mode runs an
    /// orthogonal sidecar — touch-to-mouse and gamepad presets can be
    /// stacked. Accepts a single string (`input_compat = "touch_to_mouse"`)
    /// or a list (`input_compat = ["touch_to_mouse", "gamepad_productivity"]`).
    #[serde(default, deserialize_with = "deserialize_input_compat_list")]
    pub input_compat: Vec<RawInputCompat>,

    /// Tunables for input-compat sidecars (analog deadzones, speeds).
    #[serde(default)]
    pub input_compat_options: Option<RawInputCompatOptions>,

    /// Drop the compositor output scale to 1.0 for the duration of this
    /// activity so XWayland clients render at the panel's native resolution.
    /// Sway doesn't pass scale through to XWayland (issue #45), so without
    /// this an XWayland game at `output * scale 1.5` only fills 1280x720 of
    /// a 1920x1080 panel. shepherdd compensates by telling the HUD to apply
    /// a counter-scale factor so it stays a normal size while the activity
    /// runs.
    #[serde(default)]
    pub xwayland_native_resolution: bool,
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

/// Input compatibility mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RawInputCompat {
    /// Translate touchscreen input into mouse events via a sidecar that
    /// grabs touch devices and uses the Wayland virtual-pointer protocol.
    TouchToMouse,
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
