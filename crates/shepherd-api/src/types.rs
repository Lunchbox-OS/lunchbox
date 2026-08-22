//! Shared types for the shepherdd API

use chrono::{DateTime, Local, NaiveDate};
use serde::{Deserialize, Serialize};
use shepherd_util::{EntryId, GroupId, LimitSubject, SessionId};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

/// Entry kind tag for capability matching
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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

/// How a [`EntryKind::Media`] activity opens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum MediaMode {
    /// Open the poster grid over the whole library; the user picks items.
    #[default]
    Browse,
    /// Play a single item end to end; the grid is never shown.
    Play,
}

impl MediaMode {
    /// The `shepherd-media` subcommand for this mode.
    pub fn subcommand(self) -> &'static str {
        match self {
            MediaMode::Browse => "browse",
            MediaMode::Play => "play",
        }
    }
}

/// Maximum video quality for a [`EntryKind::Media`] activity.
///
/// Mirrors `shepherd_media_app::Quality`; kept here so the wire schema and the
/// config layer don't depend on the media crates. `shepherd-media`'s `cli`
/// module holds the test that keeps the two spellings in agreement.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum MediaQuality {
    /// No height restriction — the best available.
    #[serde(rename = "best")]
    Best,
    /// Up to 1080p (default).
    #[default]
    #[serde(rename = "1080p")]
    Q1080,
    /// Up to 720p.
    #[serde(rename = "720p")]
    Q720,
    /// Up to 480p.
    #[serde(rename = "480p")]
    Q480,
}

impl MediaQuality {
    /// The `--quality` value for this preset.
    pub fn as_flag(self) -> &'static str {
        match self {
            MediaQuality::Best => "best",
            MediaQuality::Q1080 => "1080p",
            MediaQuality::Q720 => "720p",
            MediaQuality::Q480 => "480p",
        }
    }
}

/// How a [`EntryKind::Media`] activity orders its library.
///
/// Mirrors `shepherd-media`'s `--sort-by` values; see [`MediaQuality`] for
/// where that agreement is tested.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum MediaSortBy {
    /// Preserve the order from the library file or playlist (default).
    #[default]
    Library,
    /// Display title, case-insensitive.
    Title,
    /// Stable item id.
    Id,
    /// Item kind (audio before video).
    Kind,
    /// Optional category string, case-insensitive.
    Category,
    /// Optional duration in seconds, ascending.
    Duration,
}

impl MediaSortBy {
    /// The `--sort-by` value for this ordering.
    pub fn as_flag(self) -> &'static str {
        match self {
            MediaSortBy::Library => "library",
            MediaSortBy::Title => "title",
            MediaSortBy::Id => "id",
            MediaSortBy::Kind => "kind",
            MediaSortBy::Category => "category",
            MediaSortBy::Duration => "duration",
        }
    }
}

/// Entry kind with launch details
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
    /// A `shepherd-media` library activity (issue #127).
    ///
    /// The fields mirror the flags `shepherd-media` accepts, so shepherdd can
    /// build the invocation itself instead of an admin restating it as a
    /// `Process` argv. `connectivity_check` is not among them: it is resolved
    /// from the entry's `internet` policy at spawn time and reaches the host
    /// adapter through `SpawnOptions`.
    Media {
        /// Library source: a path to a `.toml`/`.m3u`/`.m3u8` file, or a
        /// YouTube playlist URL. `~` is expanded for paths at spawn time.
        library: String,
        /// Whether to open the poster grid or play a single item.
        #[serde(default)]
        mode: MediaMode,
        /// The item to play. Required by (and only meaningful for)
        /// [`MediaMode::Play`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        item: Option<String>,
        /// Maximum video quality for playback and background downloads.
        #[serde(default)]
        quality: MediaQuality,
        /// Field used to order library items before display or lookup.
        #[serde(default)]
        sort_by: MediaSortBy,
        /// Reverse the final item order. Combines with `sort_by`.
        #[serde(default)]
        reverse: bool,
        /// Remember playback positions for this library across sessions.
        #[serde(default)]
        resume: bool,
        /// Whether shepherdd may prefetch this library's remote items in the
        /// background. `None` inherits `service.media.prefetch`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prefetch: Option<bool>,
    },
    Custom {
        type_name: String,
        payload: serde_json::Value,
    },
}

/// Input compatibility mode for an activity.
///
/// Some activities don't process raw touch or gamepad events from Wayland and
/// need a shim to translate input at the compositor level. Modes are mostly
/// orthogonal: an activity can stack `TouchToMouse` (or `TabletToTouch`, or
/// `DisableTouch`) with one of the `Gamepad*` modes. The touch-handling modes
/// are the exception — `TouchToMouse`, `TabletToTouch`, and `DisableTouch` all
/// grab or produce the touchscreen, so at most one of them can be active at a
/// time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum InputCompatMode {
    /// Grab touchscreens and emit synthesized pointer events via
    /// `zwlr_virtual_pointer_v1` for the lifetime of the activity.
    TouchToMouse,
    /// Grab absolute pointers / tablets and emit synthesized touch events for
    /// activities that only handle touch input — the inverse of
    /// `TouchToMouse`. Useful for developing touch support against
    /// mouse/pen-only hardware, or VMs whose pointer is an absolute tablet.
    TabletToTouch,
    /// Grab every touchscreen and discard its events for the lifetime of the
    /// activity, effectively disabling the touchscreen. Unlike `TouchToMouse`
    /// it emits nothing — useful for activities that misbehave on touch input
    /// but should still be playable with a mouse or gamepad.
    DisableTouch,
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

    /// True if this mode grabs or produces the touchscreen. Such modes are
    /// mutually exclusive — stacking two of them would have them fight over
    /// the same devices (e.g. two `EVIOCGRAB`s) or form a loop.
    pub fn handles_touch(self) -> bool {
        matches!(
            self,
            Self::TouchToMouse | Self::TabletToTouch | Self::DisableTouch
        )
    }
}

/// A category of physical input device an activity can depend on (issue #96).
///
/// Distinct from [`InputCompatMode`], which changes how input is *translated*
/// while an activity runs. `InputDeviceType` is a *gating* concept: an activity
/// can require one or more of these device types to be connected before it is
/// shown or launchable. The canonical example is a "learn to type" activity
/// installed on a gaming handheld that should only appear once a physical
/// keyboard is attached.
///
/// Camera/microphone and MIDI are intentionally omitted for now; the issue
/// marks them as future work and this enum is closed, so configuring one is a
/// parse error rather than a silently-ignored value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum InputDeviceType {
    /// A relative pointing device (mouse, trackball, trackpad).
    Mouse,
    /// A finger touchscreen (an absolute, direct-input touch device).
    Touch,
    /// A physical alphabetic keyboard.
    Keyboard,
    /// A gamepad / game controller / joystick.
    Gamepad,
}

impl InputDeviceType {
    /// Lowercase, human-facing label ("mouse", "touch", ...). Matches the
    /// snake_case config spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mouse => "mouse",
            Self::Touch => "touch",
            Self::Keyboard => "keyboard",
            Self::Gamepad => "gamepad",
        }
    }
}

impl std::fmt::Display for InputDeviceType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How a supervised browser activity launches its window.
///
/// Translated by the host adapter into Chrome command-line flags. Shared by
/// `shepherd-config`'s validated `BrowserPolicy` and `shepherd-host-api`'s
/// `BrowserSpec` so there is a single source of truth for the mode vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum BrowserMode {
    /// Fullscreen, no browser chrome (`--kiosk`).
    Kiosk,
    /// Single application window (`--app=<url>`).
    App,
    /// Normal browser window.
    Windowed,
}

/// Per-activity tunables for input compatibility sidecars.
///
/// All fields are optional; sidecars apply their own defaults when a field is
/// `None`. Only the gamepad fields are populated today, but the struct lives
/// alongside the mode list so future tunables for other modes can be added
/// without another schema change.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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

/// A token gate's current state, for caregiver UIs (issue #8).
///
/// Banked time is a currency: source activities earn it and the gated activity
/// spends it. Without this a management UI can only report that something is
/// locked, never how close it is to unlocking, and a manual grant would be
/// made blind.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TokenStatus {
    /// Time banked and not yet spent.
    pub balance: Duration,
    /// Balance needed to open the gate. Zero means any balance opens it.
    pub minimum: Duration,
    /// Whether the gate is open right now. Not simply `balance >= minimum`:
    /// once opened it stays open until the balance is spent to zero.
    pub unlocked: bool,
    /// Ceiling on the balance. None means unlimited. A grant past this is
    /// clawed back, so a UI should say so rather than let it vanish.
    pub max_balance: Option<Duration>,
    /// Whether the balance survives local midnight.
    pub carry_over: bool,
}

/// View of an entry for UI display
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct EntryView {
    pub entry_id: EntryId,
    pub label: String,
    pub icon_ref: Option<String>,
    pub kind_tag: EntryKindTag,
    pub enabled: bool,
    /// The group this entry belongs to (issue #5), if any. Management UIs use
    /// it to show that an activity's schedule and budget are shared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<GroupId>,
    pub reasons: Vec<ReasonCode>,
    /// The entry's own token gate (issue #8), if it has one. A member of a
    /// token-gated group carries its own gate only; the category's is on the
    /// `GroupView`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<TokenStatus>,
    /// Maximum run duration if started now. None means:
    /// - If enabled=false: entry is not available
    /// - If enabled=true: entry has no time limit (unlimited)
    pub max_run_if_started_now: Option<Duration>,
}

/// View of a group for UI display (issue #5).
///
/// A group's limits are shared by its members, so a management UI needs to
/// show the *category's* state — combined usage against the combined quota,
/// and whatever is currently restricting it — separately from any one member.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct GroupView {
    pub group_id: GroupId,
    pub label: String,
    /// Members, in policy order.
    pub member_ids: Vec<EntryId>,
    /// Whether the group's own restrictions currently permit its members.
    /// Individual members may still be unavailable for their own reasons.
    pub enabled: bool,
    /// Why the group is restricting its members, if it is. These are the
    /// unwrapped reasons — the same ones members carry inside
    /// `ReasonCode::GroupRestricted`.
    pub reasons: Vec<ReasonCode>,
    /// Combined usage across all members today.
    pub used_today: Duration,
    /// Effective daily quota after any override delta. None means unlimited.
    pub daily_quota: Option<Duration>,
    /// Longest session the group's limits would currently allow a member.
    /// None means the group imposes no cap of its own.
    pub max_run_if_started_now: Option<Duration>,
    /// The category's token gate (issue #8), if it has one. Shared by every
    /// member, so it belongs here rather than on any one of them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<TokenStatus>,
}

/// Structured reason codes for why an entry is unavailable
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
    /// The activity kind has not finished warming up yet (e.g. Steam is still
    /// performing its initial load). See per-kind readiness (issue #76).
    NotReady { kind: EntryKindTag },
    /// Entry is explicitly disabled
    Disabled { reason: Option<String> },
    /// Internet connectivity is required but unavailable
    InternetUnavailable { check: Option<String> },
    /// Entry is manually disabled for the day via a daily override
    ManuallyDisabled { until: NaiveDate },
    /// One or more required input devices (issue #96) are not currently
    /// connected. `devices` lists the missing device types, sorted and
    /// deduplicated.
    RequiredInputUnavailable { devices: Vec<InputDeviceType> },
    /// A protection this entry's configuration requires cannot be applied on
    /// this host, so the entry does not launch (issue #143) — today, an
    /// `[entries.firewall]` on a host where enforcement is unavailable.
    ///
    /// Carries no detail on purpose. This is the child-facing half: to them the
    /// activity is simply unavailable, and nothing they can do changes it. The
    /// administrator-facing half — which protection, why, and how to fix it —
    /// is the matching `Diagnostic`.
    ProtectionUnavailable,
    /// Not enough time banked on this entry's token gate (issue #8): the
    /// activity has to be earned by spending time on its source activities.
    TokensInsufficient {
        /// Time currently banked toward this entry.
        balance: Duration,
        /// Balance needed before it unlocks. Zero means any balance above zero
        /// unlocks it, i.e. the entry is simply out of banked time.
        required: Duration,
    },
    /// The restriction comes from the entry's group rather than the entry
    /// itself (issue #5) — e.g. the whole category's daily quota is spent.
    /// `label` is the group's display name, for explaining it to a caregiver.
    GroupRestricted {
        group: GroupId,
        label: String,
        reason: Box<ReasonCode>,
    },
}

/// Warning severity level
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum WarningSeverity {
    Info,
    Warn,
    Critical,
}

/// Warning threshold configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct WarningThreshold {
    /// Seconds before expiry to issue this warning
    pub seconds_before: u64,
    pub severity: WarningSeverity,
    pub message_template: Option<String>,
}

/// Session end reason
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    /// Approved and spawning; the activity has not mapped a window yet.
    Launching,
    /// The activity is running normally.
    Running,
    /// Running, and at least one time warning has been issued.
    Warned,
    /// Past its deadline and being wound down.
    Expiring,
    /// Teardown has been requested and the activity is being stopped.
    ///
    /// The session is still current: the activity is on screen until the host
    /// confirms otherwise, so nothing else may launch and shells must keep the
    /// launcher out of the way. Shells should render this as a
    /// non-interactive "closing" state — without it a child gets no feedback
    /// that their press registered, which is why they pressed again on
    /// 2026-08-20 (issue #136).
    Stopping,
    /// Settled and cleared; no activity is running.
    Ended,
}

/// Active session information
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
    /// Whether the HUD should confirm before its "X" button ends this
    /// session (issue #78). Defaults to `true` when absent so older payloads
    /// keep the safe behaviour.
    #[serde(default = "default_confirm_on_close")]
    pub confirm_on_close: bool,
}

/// Default for [`SessionInfo::confirm_on_close`] / the `SessionStarted` event:
/// confirmation is enabled unless a config explicitly opts out.
pub(crate) fn default_confirm_on_close() -> bool {
    true
}

/// Status of a single internet connectivity check target
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct InternetStatusView {
    /// Original check string as configured (e.g. "https://example.com")
    pub target: String,
    /// Whether the last check succeeded
    pub available: bool,
}

/// Full service state snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
    /// Administrator-facing conditions currently true of this device (issue
    /// #143) — a missing dependency, a protection that is not in effect. Rides
    /// the snapshot so every client has the current set on subscribe; deltas
    /// arrive as `EventPayload::DiagnosticsChanged`.
    #[serde(default)]
    pub diagnostics: crate::DiagnosticSet,
}

/// Role for authorization
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum StopMode {
    /// Try graceful termination first
    Graceful,
    /// Force immediate termination
    Force,
}

/// Health status
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct HealthStatus {
    pub live: bool,
    pub ready: bool,
    pub policy_loaded: bool,
    pub host_adapter_ok: bool,
    pub store_ok: bool,
}

/// What kind of thing an audio output is.
///
/// Advisory only: it drives presentation (an icon, a label) and never policy.
/// It cannot be determined for every device — a generic USB interface reports a
/// nondescript `analog-output` route and no udev form-factor — so `Unknown` is a
/// routine outcome, not a failure.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum AudioOutputKind {
    Speakers,
    Headphones,
    Hdmi,
    Digital,
    LineOut,
    Bluetooth,
    #[default]
    Unknown,
}

/// The audio output a volume reading applies to.
///
/// `key` is `<device.name>:output:<route.name>` — the same key WirePlumber uses
/// to persist per-route volume, so our notion of "an output" cannot drift from
/// the volume PipeWire remembers for it. It is stable across reboots and, for
/// USB devices, across being moved to a different port.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct AudioOutput {
    /// Stable identity. Use this to correlate, never the description.
    pub key: String,
    /// Human-readable label for display. Localized and mutable.
    pub description: String,
    pub kind: AudioOutputKind,
}

/// Volume status information
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
    /// The output this reading applies to. `None` on hosts without PipeWire, or
    /// when the default sink cannot be resolved to a known output.
    #[serde(default)]
    pub output: Option<AudioOutput>,
}

/// Volume restrictions that are currently in effect
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
    /// Whether an ambient light sensor is present, so automatic brightness
    /// can be offered at all. When false, `auto_enabled` is always false.
    #[serde(default)]
    pub auto_available: bool,
    /// Whether automatic (ambient-light) brightness is currently enabled.
    #[serde(default)]
    pub auto_enabled: bool,
}

/// Brightness restrictions that are currently in effect
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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

/// A compositor output video mode: pixel resolution and refresh rate.
///
/// `refresh_mhz` is millihertz, matching sway's `get_outputs` JSON (60 Hz is
/// `60000`). Refresh participates in equality, but [`VideoMode::area`] ignores
/// it so "highest resolution" comparisons are purely by pixel count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct VideoMode {
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub refresh_mhz: u32,
}

impl VideoMode {
    /// Pixel area, used to rank modes by resolution.
    pub fn area(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }
}

/// How the kiosk drives displays when an external monitor is docked (issue #87).
///
/// Exactly one logical output is ever active in every variant, so the
/// one-activity-at-a-time invariant always holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum DisplayMode {
    /// Only the internal/primary panel is active — the state when no external
    /// display is connected.
    SingleInternal,
    /// The external display mirrors the primary. Default whenever an external
    /// display connects.
    Mirror,
    /// The primary panel is disabled and the external display drives the
    /// session at its native resolution.
    ExternalOnly,
}

impl DisplayMode {
    /// The mode the HUD toggle flips to from the current one. `Mirror` and
    /// `ExternalOnly` toggle between each other; `SingleInternal` has no
    /// external display to toggle, so it maps to itself.
    pub fn toggled(self) -> Self {
        match self {
            DisplayMode::Mirror => DisplayMode::ExternalOnly,
            DisplayMode::ExternalOnly => DisplayMode::Mirror,
            DisplayMode::SingleInternal => DisplayMode::SingleInternal,
        }
    }
}

/// Snapshot of the compositor's display arrangement, broadcast to shells so the
/// HUD can show/hide and label its mirror/external toggle (issue #87).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DisplayState {
    pub mode: DisplayMode,
    /// Connector name of the primary (internal, first-enumerated) output.
    pub primary: Option<String>,
    /// Connector name of the external/secondary output, if one is connected.
    pub secondary: Option<String>,
}

impl DisplayState {
    /// True when an external display is connected — the condition under which
    /// the HUD reveals its mode toggle.
    pub fn has_secondary(&self) -> bool {
        self.secondary.is_some()
    }
}

/// A parent-set daily override for a single entry
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct DailyOverride {
    /// What the override applies to: an entry, or a whole group (issue #5).
    /// Serializes as a bare entry ID, or `group:<id>` for a group, so overrides
    /// written before groups existed round-trip unchanged.
    pub subject: LimitSubject,
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
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct UsageStat {
    pub entry_id: EntryId,
    pub label: String,
    pub date: NaiveDate,
    pub duration_seconds: u64,
}

/// An action that can be performed on a window via the debug API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum WindowAction {
    /// Ask the window to close (sway `kill`).
    Close,
    /// Move the window to the scratchpad to hide it from view.
    Hide,
    /// Pull the window out of the scratchpad so it is shown again.
    Show,
}

/// Who shepherd believes a window belongs to.
///
/// The compositor cannot answer this — it reports pids, not intent. The host
/// fills it in by matching each window against what it is actually
/// supervising, which is what lets an admin UI tell "the game the child is
/// playing" apart from "something on the screen that no session owns".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum WindowOwner {
    /// Shepherd's own furniture: the launcher, the HUD, the pairing UI, the
    /// mirror, and background processes it keeps warm (the preloaded Steam
    /// client). Expected to outlive every session.
    Shepherd,
    /// A process shepherd is supervising for the current session — the
    /// activity itself, something in its process group, a Steam game
    /// launched on its behalf, or one of its input sidecars.
    Activity,
    /// An activity that outlived its own teardown. Its session is over and
    /// the host is still working on killing it — the same condition that
    /// writes an `ActivityEscaped` audit record.
    Escaped,
    /// No process shepherd knows about. Either something started outside
    /// shepherd entirely, or an activity that got away without the host ever
    /// noticing — the case supervision cannot fix on its own, and the reason
    /// this field exists.
    Unowned,
}

/// Debug snapshot of a single window known to the host's compositor.
///
/// Currently surfaced via the management API for debugging the Sway tree —
/// in particular, to see which windows have been moved to the scratchpad
/// (e.g. the hidden Steam client) versus which are on-screen.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
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
    /// What shepherd is supervising behind this window, if anything.
    ///
    /// A host that cannot attribute windows reports every one of them as
    /// unowned.
    pub owner: WindowOwner,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The wire spellings the web UI and the companion app switch on. Both
    /// clients hard-code these strings, and a silent rename here would
    /// downgrade every orphan to an ordinary row rather than failing.
    #[test]
    fn window_owner_wire_spelling() {
        let spellings = [
            (WindowOwner::Shepherd, "\"shepherd\""),
            (WindowOwner::Activity, "\"activity\""),
            (WindowOwner::Escaped, "\"escaped\""),
            (WindowOwner::Unowned, "\"unowned\""),
        ];
        for (owner, json) in spellings {
            assert_eq!(serde_json::to_string(&owner).unwrap(), json);
            assert_eq!(serde_json::from_str::<WindowOwner>(json).unwrap(), owner);
        }
    }

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
