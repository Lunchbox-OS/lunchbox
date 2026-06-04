//! Shared types for the shepherdd API

use chrono::{DateTime, Local, NaiveDate};
use serde::{Deserialize, Serialize};
use shepherd_util::{EntryId, SessionId};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

/// Entry kind tag for capability matching
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKindTag {
    Process,
    Snap,
    Steam,
    Flatpak,
    Vm,
    Media,
    Custom,
}

/// A known Steam "launch interstitial" — one of the blocking modals Steam can
/// show between a launch request and the game actually starting (cloud-sync
/// warnings, controller advisories, etc.). The kiosk can be configured to
/// auto-dismiss specific kinds; see `service.steam.auto_dismiss_interstitials`.
///
/// This enum is the canonical catalog: config validates against it, and the
/// host adapter attaches the per-kind CEF detection signatures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterstitialKind {
    /// "Unable to Sync" Steam Cloud warning shown when launching offline with
    /// un-uploaded saves. Affirmative action: "Play anyway". (Verified.)
    CloudSync,
    /// "Grab a controller…" advisory for controller-recommended games launched
    /// without a controller. Affirmative action: "OK". (Verified.)
    ControllerRecommended,
    /// First-launch "intro to Steam Input" notice. Affirmative action: "OK".
    /// (Best-effort signature.)
    SteamInputIntro,
    /// Game *requires* a controller. Dismissing launches a game that cannot be
    /// played without one, so this is risky. (Best-effort signature.)
    ControllerRequired,
    /// Game requires a VR headset. Dismissing launches something unusable
    /// without VR hardware, so this is risky. (Best-effort signature.)
    VrRequired,
}

impl InterstitialKind {
    /// Every known kind, in catalog order.
    pub const ALL: [InterstitialKind; 5] = [
        InterstitialKind::CloudSync,
        InterstitialKind::ControllerRecommended,
        InterstitialKind::SteamInputIntro,
        InterstitialKind::ControllerRequired,
        InterstitialKind::VrRequired,
    ];

    /// The kinds auto-dismissed by default: the verified, benign ones.
    pub const DEFAULT_AUTO_DISMISS: [InterstitialKind; 2] = [
        InterstitialKind::CloudSync,
        InterstitialKind::ControllerRecommended,
    ];

    /// Stable config slug for this kind (matches the serde snake_case name).
    pub fn slug(self) -> &'static str {
        match self {
            InterstitialKind::CloudSync => "cloud_sync",
            InterstitialKind::ControllerRecommended => "controller_recommended",
            InterstitialKind::SteamInputIntro => "steam_input_intro",
            InterstitialKind::ControllerRequired => "controller_required",
            InterstitialKind::VrRequired => "vr_required",
        }
    }

    /// Parse a config slug into a kind.
    pub fn from_slug(slug: &str) -> Option<Self> {
        InterstitialKind::ALL.into_iter().find(|k| k.slug() == slug)
    }

    /// Whether auto-dismissing this kind launches something the user likely
    /// can't actually use (missing required hardware). Risky kinds require an
    /// explicit opt-in to enable.
    pub fn is_risky(self) -> bool {
        matches!(
            self,
            InterstitialKind::ControllerRequired | InterstitialKind::VrRequired
        )
    }
}

/// Entry kind with launch details
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EntryKind {
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
        #[serde(default)]
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
        payload: serde_json::Value,
    },
}

/// Input compatibility mode for an activity.
///
/// Some activities don't process raw touch or gamepad events from Wayland and
/// need a shim to translate input at the compositor level. Modes are
/// orthogonal: an activity can stack `TouchToMouse` with one of the
/// `Gamepad*` modes if the device has both a touchscreen and a gamepad.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputCompatMode {
    /// Grab touchscreens and emit synthesized pointer events via
    /// `zwlr_virtual_pointer_v1` for the lifetime of the activity.
    TouchToMouse,
    /// Remap a gamepad to mouse + keyboard using the productivity preset:
    /// triggers = LMB, shoulders = RMB, left stick = mouse, right stick =
    /// scroll, stick-click toggles which stick drives the mouse, D-pad =
    /// arrow keys, A = Enter, Start = Escape.
    GamepadProductivity,
    /// Remap a gamepad to mouse + keyboard using the GPD/FPS preset:
    /// LT = LMB, RT = RMB, LB = MMB, left stick = WASD, right stick = mouse,
    /// D-pad = scroll, A = Space, X = R, B = E, Y = F.
    GamepadGpd,
}

impl InputCompatMode {
    /// True if this mode is one of the gamepad presets.
    pub fn is_gamepad(self) -> bool {
        matches!(self, Self::GamepadProductivity | Self::GamepadGpd)
    }
}

/// Per-activity tunables for input compatibility sidecars.
///
/// All fields are optional; sidecars apply their own defaults when a field is
/// `None`. Only the gamepad fields are populated today, but the struct lives
/// alongside the mode list so future tunables for other modes can be added
/// without another schema change.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct InputCompatOptions {
    /// Gamepad analog-stick deadzone as a fraction of full deflection (0..1).
    /// Below this magnitude the stick is treated as centered.
    pub gamepad_deadzone: Option<f32>,
    /// Gamepad mouse speed in pixels per second at full stick deflection.
    pub gamepad_mouse_speed: Option<f32>,
    /// Gamepad scroll speed in discrete wheel units per second at full
    /// deflection.
    pub gamepad_scroll_speed: Option<f32>,
}

impl InputCompatOptions {
    /// True if every field is `None`.
    pub fn is_empty(&self) -> bool {
        self.gamepad_deadzone.is_none()
            && self.gamepad_mouse_speed.is_none()
            && self.gamepad_scroll_speed.is_none()
    }
}

impl EntryKind {
    pub fn tag(&self) -> EntryKindTag {
        match self {
            EntryKind::Process { .. } => EntryKindTag::Process,
            EntryKind::Snap { .. } => EntryKindTag::Snap,
            EntryKind::Steam { .. } => EntryKindTag::Steam,
            EntryKind::Flatpak { .. } => EntryKindTag::Flatpak,
            EntryKind::Vm { .. } => EntryKindTag::Vm,
            EntryKind::Media { .. } => EntryKindTag::Media,
            EntryKind::Custom { .. } => EntryKindTag::Custom,
        }
    }
}

/// View of an entry for UI display
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntryView {
    pub entry_id: EntryId,
    pub label: String,
    pub icon_ref: Option<String>,
    pub kind_tag: EntryKindTag,
    pub enabled: bool,
    pub reasons: Vec<ReasonCode>,
    /// Maximum run duration if started now. None means:
    /// - If enabled=false: entry is not available
    /// - If enabled=true: entry has no time limit (unlimited)
    pub max_run_if_started_now: Option<Duration>,
}

/// Structured reason codes for why an entry is unavailable
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum ReasonCode {
    /// Outside allowed time window
    OutsideTimeWindow {
        /// When the next window opens (if known)
        next_window_start: Option<DateTime<Local>>,
    },
    /// Daily quota exhausted
    QuotaExhausted { used: Duration, quota: Duration },
    /// Cooldown period active
    CooldownActive { available_at: DateTime<Local> },
    /// Another session is active
    SessionActive {
        entry_id: EntryId,
        /// Time remaining in current session. None means unlimited.
        remaining: Option<Duration>,
    },
    /// Host doesn't support this entry kind
    UnsupportedKind { kind: EntryKindTag },
    /// Entry is explicitly disabled
    Disabled { reason: Option<String> },
    /// Internet connectivity is required but unavailable
    InternetUnavailable { check: Option<String> },
    /// Entry is manually disabled for the day via a daily override
    ManuallyDisabled { until: NaiveDate },
}

/// Warning severity level
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WarningSeverity {
    Info,
    Warn,
    Critical,
}

/// Warning threshold configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WarningThreshold {
    /// Seconds before expiry to issue this warning
    pub seconds_before: u64,
    pub severity: WarningSeverity,
    pub message_template: Option<String>,
}

/// Session end reason
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEndReason {
    /// Session expired (time limit reached)
    Expired,
    /// User requested stop
    UserStop,
    /// Admin requested stop
    AdminStop,
    /// Process exited on its own
    ProcessExited { exit_code: Option<i32> },
    /// Policy change terminated session
    PolicyStop,
    /// Service shutdown
    ServiceShutdown,
    /// Launch failed
    LaunchFailed { error: String },
}

/// Current session state
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Launching,
    Running,
    Warned,
    Expiring,
    Ended,
}

/// Active session information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session_id: SessionId,
    pub entry_id: EntryId,
    pub label: String,
    pub state: SessionState,
    pub started_at: DateTime<Local>,
    /// Session deadline. None means unlimited (no time limit).
    pub deadline: Option<DateTime<Local>>,
    /// Time remaining. None means unlimited.
    pub time_remaining: Option<Duration>,
    pub warnings_issued: Vec<u64>,
}

/// Status of a single internet connectivity check target
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InternetStatusView {
    /// Original check string as configured (e.g. "https://example.com")
    pub target: String,
    /// Whether the last check succeeded
    pub available: bool,
}

/// Full service state snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceStateSnapshot {
    pub api_version: u32,
    pub policy_loaded: bool,
    pub current_session: Option<SessionInfo>,
    pub entry_count: usize,
    /// Available entries for UI display
    #[serde(default)]
    pub entries: Vec<EntryView>,
    /// Latest known status of each configured internet connectivity check.
    /// Empty when no connectivity checks are configured.
    #[serde(default)]
    pub internet_status: Vec<InternetStatusView>,
}

/// Role for authorization
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientRole {
    /// UI/HUD - can view state, launch entries, stop current
    Shell,
    /// Local admin - can also extend, reload config
    Admin,
    /// Read-only observer
    Observer,
}

impl ClientRole {
    pub fn can_launch(&self) -> bool {
        matches!(self, ClientRole::Shell | ClientRole::Admin)
    }

    pub fn can_stop(&self) -> bool {
        matches!(self, ClientRole::Shell | ClientRole::Admin)
    }

    pub fn can_extend(&self) -> bool {
        matches!(self, ClientRole::Admin)
    }

    pub fn can_reload_config(&self) -> bool {
        matches!(self, ClientRole::Admin)
    }
}

/// Stop mode for session termination
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopMode {
    /// Try graceful termination first
    Graceful,
    /// Force immediate termination
    Force,
}

/// Health status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthStatus {
    pub live: bool,
    pub ready: bool,
    pub policy_loaded: bool,
    pub host_adapter_ok: bool,
    pub store_ok: bool,
}

/// Volume status information
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VolumeInfo {
    /// Volume percentage (0-100)
    pub percent: u8,
    /// Whether audio is muted
    pub muted: bool,
    /// Whether volume control is available
    pub available: bool,
    /// The detected sound backend (e.g., "pipewire", "pulseaudio", "alsa")
    pub backend: Option<String>,
    /// Current restrictions on volume
    pub restrictions: VolumeRestrictions,
}

/// Volume restrictions that are currently in effect
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VolumeRestrictions {
    /// Maximum volume percentage allowed
    pub max_volume: Option<u8>,
    /// Minimum volume percentage allowed
    pub min_volume: Option<u8>,
    /// Whether mute toggle is allowed
    pub allow_mute: bool,
    /// Whether volume changes are allowed at all
    pub allow_change: bool,
}

impl VolumeRestrictions {
    /// Create unrestricted volume settings
    pub fn unrestricted() -> Self {
        Self {
            max_volume: None,
            min_volume: None,
            allow_mute: true,
            allow_change: true,
        }
    }

    /// Clamp a volume value to the allowed range
    pub fn clamp_volume(&self, percent: u8) -> u8 {
        let min = self.min_volume.unwrap_or(0);
        let max = self.max_volume.unwrap_or(100);
        percent.clamp(min, max)
    }
}

impl VolumeInfo {
    /// Get an icon name for the current volume status
    pub fn icon_name(&self) -> &'static str {
        if self.muted || self.percent == 0 {
            "audio-volume-muted-symbolic"
        } else if self.percent < 33 {
            "audio-volume-low-symbolic"
        } else if self.percent < 66 {
            "audio-volume-medium-symbolic"
        } else {
            "audio-volume-high-symbolic"
        }
    }
}

/// Screen brightness status information
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BrightnessInfo {
    /// Brightness percentage (0-100)
    pub percent: u8,
    /// Whether brightness control is available on this host
    pub available: bool,
    /// The detected brightness backend (e.g. "sysfs", "brightnessctl")
    pub backend: Option<String>,
    /// Name of the backlight device being controlled, if any
    pub device: Option<String>,
    /// Current restrictions on brightness
    pub restrictions: BrightnessRestrictions,
}

/// Brightness restrictions that are currently in effect
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BrightnessRestrictions {
    /// Maximum brightness percentage allowed
    pub max_brightness: Option<u8>,
    /// Minimum brightness percentage allowed
    pub min_brightness: Option<u8>,
    /// Whether brightness changes are allowed at all
    pub allow_change: bool,
}

impl BrightnessRestrictions {
    /// Create unrestricted brightness settings
    pub fn unrestricted() -> Self {
        Self {
            max_brightness: None,
            min_brightness: None,
            allow_change: true,
        }
    }

    /// Clamp a brightness value to the allowed range
    pub fn clamp_brightness(&self, percent: u8) -> u8 {
        let min = self.min_brightness.unwrap_or(0);
        let max = self.max_brightness.unwrap_or(100);
        percent.clamp(min, max)
    }
}

impl BrightnessInfo {
    /// Get an icon name for the current brightness status.
    //
    // Adwaita and Yaru only ship a single `display-brightness-symbolic`
    // glyph; the percentage-tiered `*-low/medium/high-symbolic` names
    // that older GNOME themes used no longer resolve, so the HUD would
    // render the missing-image placeholder if we returned those.
    pub fn icon_name(&self) -> &'static str {
        "display-brightness-symbolic"
    }
}

/// A parent-set daily override for a single entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyOverride {
    pub entry_id: EntryId,
    pub date: NaiveDate,
    /// Override the entry's availability for this day.
    /// `Some(false)` blocks it entirely; `Some(true)` allows it outside its time window.
    /// `None` means no availability override (quota delta may still apply).
    pub availability: Option<bool>,
    /// Signed adjustment to today's quota in seconds.
    /// Positive = extra time; negative = reduced time; `None` = no change.
    pub quota_delta_seconds: Option<i64>,
    pub created_at: DateTime<Local>,
    pub updated_at: DateTime<Local>,
}

/// Screen-time usage for a single entry on a single day
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageStat {
    pub entry_id: EntryId,
    pub label: String,
    pub date: NaiveDate,
    pub duration_seconds: u64,
}

/// An action that can be performed on a window via the debug API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowAction {
    /// Ask the window to close (sway `kill`).
    Close,
    /// Move the window to the scratchpad to hide it from view.
    Hide,
    /// Pull the window out of the scratchpad so it is shown again.
    Show,
}

/// Debug snapshot of a single window known to the host's compositor.
///
/// Currently surfaced via the management API for debugging the Sway tree —
/// in particular, to see which windows have been moved to the scratchpad
/// (e.g. the hidden Steam client) versus which are on-screen.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowInfo {
    /// Compositor-assigned window/container id.
    pub id: u64,
    /// Window title, if the application set one.
    pub name: Option<String>,
    /// Wayland app_id, if available.
    pub app_id: Option<String>,
    /// X11 class (xwayland windows), if available.
    pub window_class: Option<String>,
    /// Owning process id, if reported by the compositor.
    pub pid: Option<u32>,
    /// Workspace name the window belongs to, if any. `__i3_scratch` is the
    /// scratchpad pseudo-workspace.
    pub workspace: Option<String>,
    /// True if the window currently lives on the scratchpad (hidden).
    pub in_scratchpad: bool,
    /// True if the window is currently being rendered.
    pub visible: bool,
    /// True if the window has keyboard focus.
    pub focused: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_kind_serialization() {
        let kind = EntryKind::Process {
            command: "scummvm".into(),
            args: vec!["-f".into()],
            env: HashMap::new(),
            cwd: None,
        };

        let json = serde_json::to_string(&kind).unwrap();
        let parsed: EntryKind = serde_json::from_str(&json).unwrap();

        assert_eq!(kind, parsed);
    }

    #[test]
    fn reason_code_serialization() {
        let reason = ReasonCode::QuotaExhausted {
            used: Duration::from_secs(3600),
            quota: Duration::from_secs(3600),
        };

        let json = serde_json::to_string(&reason).unwrap();
        assert!(json.contains("quota_exhausted"));
    }
}
