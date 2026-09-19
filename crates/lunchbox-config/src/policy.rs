//! Validated policy structures

use crate::icon::autodetect_icon;
use crate::internet::{
    DEFAULT_INTERNET_CHECK_INTERVAL, DEFAULT_INTERNET_CHECK_TIMEOUT, EntryInternetPolicy,
    InternetCheckTarget, InternetConfig,
};
use crate::schema::{
    RawAutoBrightnessConfig, RawBleManagementConfig, RawBrightnessConfig, RawBrowserConfig,
    RawConfig, RawEntry, RawEntryKind, RawFirewallConfig, RawHudOrientation, RawInputCompat,
    RawInputCompatOptions, RawInputDevice, RawInternetConfig, RawManagementApiConfig, RawMediaMode,
    RawMediaQuality, RawMediaSortBy, RawServiceConfig, RawSteamConfig, RawVolumeConfig,
    RawWarningThreshold,
};
use crate::validation::{parse_days, parse_firewall_rule, parse_time};
use lunchbox_api::{
    BrowserMode, EntryKind, HudOrientation, InputCompatMode, InputCompatOptions, InputDeviceType,
    InterstitialKind, MediaMode, MediaQuality, MediaSortBy, WarningSeverity, WarningThreshold,
};
use lunchbox_util::{
    DaysOfWeek, EntryId, GroupId, LimitSubject, TimeWindow, WallClock, default_data_dir,
    default_log_dir, socket_path_without_env,
};
use std::collections::HashSet;
use std::net::IpAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

/// Default minimum session length before a cooldown is started.
///
/// Activities that crash or bounce seconds after launch would otherwise start
/// their cooldown and lock the child out of an activity they never got to use,
/// so sessions shorter than this leave the cooldown alone. Configurable with
/// `service.cooldown_min_session_seconds`, per subject with
/// `limits.cooldown_min_session_seconds`.
pub const DEFAULT_COOLDOWN_MIN_SESSION: Duration = Duration::from_secs(120);

/// Default time a running activity gets to save its progress when a resume
/// from sleep lands outside its allowed hours (issue #155).
///
/// The clock is monotonic, so sleeping does not spend the session's budget —
/// but it does move the session past the wall-clock window that bounded it.
/// Rather than cut the activity dead the instant the machine wakes, the child
/// gets this long, with a warning, to put their progress somewhere safe.
/// Configurable with `service.save_grace_seconds`, per entry with
/// `limits.save_grace_seconds`.
pub const DEFAULT_SAVE_GRACE: Duration = Duration::from_secs(120);

/// Default per-session cap for an entry that sets no `max_run_seconds` and
/// whose service sets no `default_max_run_seconds`.
///
/// A cap rather than "unlimited": an activity nobody thought about is still
/// bounded. Configurable with `service.default_max_run_seconds`, per subject
/// with `limits.max_run_seconds`, and `0` in either means unlimited.
pub const DEFAULT_MAX_RUN: Duration = Duration::from_secs(3600);

/// Default TCP port for the management API.
pub const DEFAULT_MANAGEMENT_API_PORT: u16 = 7890;

/// Default bind address for the management API — loopback, so enabling it
/// without a token does not expose the device to the network.
pub const DEFAULT_MANAGEMENT_API_BIND: &str = "127.0.0.1";

/// Default window for retrying the management API's initial bind, for an
/// interface that is not up yet. `bind_retry_seconds = 0` retries forever.
pub const DEFAULT_MANAGEMENT_API_BIND_RETRY: Duration = Duration::from_secs(300);

/// Default idle timeout for a web management session (issue #156).
pub const DEFAULT_SESSION_IDLE: Duration = Duration::from_secs(2 * 24 * 3600);

/// Default absolute lifetime for a web management session, however active.
pub const DEFAULT_SESSION_MAX_AGE: Duration = Duration::from_secs(14 * 24 * 3600);

/// Default consecutive failed logins from one address before a lockout.
pub const DEFAULT_LOCKOUT_AFTER: u32 = 8;

/// Default length of a first lockout; each subsequent one doubles, to 16x.
pub const DEFAULT_LOCKOUT: Duration = Duration::from_secs(300);

/// Default seconds banked per second spent on a token gate's source activity.
pub const DEFAULT_TOKEN_EARN_RATIO: f64 = 1.0;

/// The service-level defaults a subject's `[limits]` table falls back to.
///
/// Bundled rather than passed as three loose `Duration`s: two of them have the
/// same type, and a swapped pair would silently mis-resolve every entry.
#[derive(Debug, Clone, Copy)]
pub struct LimitDefaults {
    /// Default per-session cap. `None` means unlimited. Groups pass `None`
    /// regardless: an absent group limit means "no group-level cap".
    pub max_run: Option<Duration>,
    /// Default minimum session length before a cooldown starts.
    pub cooldown_min_session: Duration,
    /// Default save-progress grace on a resume outside allowed hours.
    pub save_grace: Duration,
}

/// Validated policy ready for use by the core engine
#[derive(Debug, Clone)]
pub struct Policy {
    /// Service configuration
    pub service: ServiceConfig,

    /// Validated entries
    pub entries: Vec<Entry>,

    /// Validated groups (issue #5). Entries reference these by ID.
    pub groups: Vec<Group>,

    /// Default warning thresholds
    pub default_warnings: Vec<WarningThreshold>,

    /// Default max run duration. None means unlimited.
    pub default_max_run: Option<Duration>,

    /// Global volume restrictions
    pub volume: VolumePolicy,

    /// Global screen-brightness restrictions
    pub brightness: BrightnessPolicy,

    /// Automatic (ambient-light) brightness settings (device-global)
    pub auto_brightness: AutoBrightnessPolicy,

    /// Which screen edge the HUD occupies by default (issue #171). An entry
    /// with its own `hud_orientation` overrides this for the life of its
    /// session.
    pub hud_orientation: HudOrientation,
}

impl Policy {
    /// Convert from raw config (after validation)
    pub fn from_raw(raw: RawConfig) -> Self {
        let default_warnings = raw
            .service
            .default_warnings
            .clone()
            .map(|w| w.into_iter().map(convert_warning).collect())
            .unwrap_or_else(default_warning_thresholds);

        // 0 means unlimited, None means use DEFAULT_MAX_RUN
        let default_max_run = raw
            .service
            .default_max_run_seconds
            .map(seconds_to_duration_or_unlimited)
            .unwrap_or(Some(DEFAULT_MAX_RUN));

        let limit_defaults = LimitDefaults {
            max_run: default_max_run,
            cooldown_min_session: raw
                .service
                .cooldown_min_session_seconds
                .map(Duration::from_secs)
                .unwrap_or(DEFAULT_COOLDOWN_MIN_SESSION),
            save_grace: raw
                .service
                .save_grace_seconds
                .map(Duration::from_secs)
                .unwrap_or(DEFAULT_SAVE_GRACE),
        };

        let global_volume = raw
            .service
            .volume
            .as_ref()
            .map(convert_volume_config)
            .unwrap_or_default();

        let global_brightness = raw
            .service
            .brightness
            .as_ref()
            .map(convert_brightness_config)
            .unwrap_or_default();

        let auto_brightness = raw
            .service
            .brightness
            .as_ref()
            .and_then(|b| b.auto.as_ref())
            .map(convert_auto_brightness_config)
            .unwrap_or_default();

        let hud_orientation = raw
            .service
            .hud
            .as_ref()
            .and_then(|h| h.orientation)
            .map(convert_hud_orientation)
            .unwrap_or_default();

        let groups: Vec<Group> = raw
            .groups
            .iter()
            .map(|g| Group::from_raw(g, limit_defaults))
            .collect();

        let entries = raw
            .entries
            .into_iter()
            .map(|e| {
                // The save-progress grace is the one limit that cascades
                // service -> group -> entry, because unlike the others it
                // describes a single session rather than a budget: a "bedtime"
                // category sets how long its activities get to save, once, and
                // there is no sense in which an entry and its group could each
                // be held to their own answer.
                let defaults = match e
                    .group
                    .as_deref()
                    .and_then(|id| groups.iter().find(|g| g.id.as_str() == id))
                {
                    Some(group) => LimitDefaults {
                        save_grace: group.limits.save_grace,
                        ..limit_defaults
                    },
                    None => limit_defaults,
                };
                Entry::from_raw(
                    e,
                    &default_warnings,
                    defaults,
                    &global_volume,
                    &global_brightness,
                )
            })
            .collect();

        Self {
            service: ServiceConfig::from_raw(raw.service),
            entries,
            groups,
            default_warnings,
            default_max_run,
            volume: global_volume,
            brightness: global_brightness,
            auto_brightness,
            hud_orientation,
        }
    }

    /// Get entry by ID
    pub fn get_entry(&self, id: &EntryId) -> Option<&Entry> {
        self.entries.iter().find(|e| &e.id == id)
    }

    /// Get group by ID
    pub fn get_group(&self, id: &GroupId) -> Option<&Group> {
        self.groups.iter().find(|g| &g.id == id)
    }

    /// The group an entry belongs to, if any.
    pub fn group_of(&self, entry: &Entry) -> Option<&Group> {
        entry.group.as_ref().and_then(|id| self.get_group(id))
    }

    /// The token gate on a limit subject — an entry's own, or a group's
    /// (issue #8). None when the subject doesn't exist or isn't gated.
    pub fn tokens_of(&self, subject: &LimitSubject) -> Option<&TokensPolicy> {
        match subject {
            LimitSubject::Entry(id) => self.get_entry(id).and_then(|e| e.tokens.as_ref()),
            LimitSubject::Group(id) => self.get_group(id).and_then(|g| g.tokens.as_ref()),
        }
    }

    /// Every entry belonging to a group.
    pub fn group_members(&self, id: &GroupId) -> impl Iterator<Item = &Entry> {
        self.entries
            .iter()
            .filter(move |e| e.group.as_ref() == Some(id))
    }
}

/// A group of entries sharing an availability schedule and limits (issue #5).
///
/// The group's daily quota is spent by the *combined* usage of its members, so
/// one member can burn the whole category's budget; when it is gone, every
/// member becomes unavailable at once.
#[derive(Debug, Clone)]
pub struct Group {
    pub id: GroupId,
    pub label: String,
    pub availability: AvailabilityPolicy,
    pub limits: LimitsPolicy,
    /// Token gate on the whole group: earning unlocks every member.
    pub tokens: Option<TokensPolicy>,
}

impl Group {
    fn from_raw(raw: &crate::schema::RawGroup, defaults: LimitDefaults) -> Self {
        // Unlike an entry, a group has no service-level max_run default: an
        // absent group limit means "no group-level cap", not "one hour".
        let defaults = LimitDefaults {
            max_run: None,
            ..defaults
        };
        Self {
            id: GroupId::new(raw.id.clone()),
            label: raw.label.clone(),
            availability: raw
                .availability
                .clone()
                .map(convert_availability)
                .unwrap_or_default(),
            limits: raw
                .limits
                .clone()
                .map(|l| convert_limits(l, defaults))
                .unwrap_or(LimitsPolicy {
                    max_run: None,
                    daily_quota: None,
                    cooldown: None,
                    cooldown_min_session: defaults.cooldown_min_session,
                    save_grace: defaults.save_grace,
                }),
            tokens: raw.tokens.as_ref().map(convert_tokens),
        }
    }

    /// This group as a limit subject, for cooldown / token / override lookups.
    pub fn subject(&self) -> LimitSubject {
        LimitSubject::Group(self.id.clone())
    }
}

/// Service configuration
#[derive(Debug, Clone)]
pub struct ServiceConfig {
    pub socket_path: PathBuf,
    pub log_dir: PathBuf,
    pub data_dir: PathBuf,
    /// Whether to capture stdout/stderr from child applications
    pub capture_child_output: bool,
    /// Directory for child application logs
    pub child_log_dir: PathBuf,
    /// Internet connectivity configuration
    pub internet: InternetConfig,
    /// Steam-specific behaviour
    pub steam: SteamConfig,
    /// Management HTTP API configuration (None = disabled)
    pub management_api: Option<ManagementApiConfig>,
    /// Bluetooth LE management transport configuration (None = disabled).
    pub ble_management: Option<BleManagementConfig>,
    /// External monitor / docking behaviour (issue #87).
    pub display: DisplayConfig,
    /// Background media prefetch (issue #127).
    pub media: MediaServiceConfig,
    /// Remote file management over the web interface (issue #195).
    pub file_manager: FileManagerConfig,
}

/// Validated remote file manager settings (issue #195).
#[derive(Debug, Clone)]
pub struct FileManagerConfig {
    /// Whether the file routes are served at all.
    pub enabled: bool,
    /// Largest single upload, in bytes. 0 removes the cap.
    pub max_upload_bytes: u64,
    /// Free-space floor on the destination filesystem, in bytes. 0 disables
    /// the check.
    pub free_space_floor_bytes: u64,
    /// Whether removable drives under `/media` and `/run/media` are offered.
    pub external_media: bool,
    /// Extra directories to offer, already checked for absoluteness and for
    /// not being a system directory.
    pub extra_roots: Vec<FileManagerRoot>,
}

/// One extra place the file manager may browse (issue #195).
#[derive(Debug, Clone)]
pub struct FileManagerRoot {
    /// What the web interface calls it.
    pub label: String,
    /// Absolute path on this device.
    pub path: PathBuf,
}

impl Default for FileManagerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_upload_bytes: 8 * 1024 * 1024 * 1024,
            free_space_floor_bytes: 2 * 1024 * 1024 * 1024,
            external_media: true,
            extra_roots: Vec::new(),
        }
    }
}

impl FileManagerConfig {
    fn from_raw(raw: Option<&crate::schema::RawFileManagerConfig>) -> Self {
        match raw {
            Some(r) => Self {
                enabled: r.enabled,
                max_upload_bytes: r.max_upload_bytes,
                free_space_floor_bytes: r.free_space_floor_bytes,
                external_media: r.external_media,
                extra_roots: r
                    .extra_roots
                    .iter()
                    .map(|root| FileManagerRoot {
                        label: root.label.clone(),
                        path: root.path.clone(),
                    })
                    .collect(),
            },
            None => Self::default(),
        }
    }
}

/// Validated service-wide media behaviour (issue #127).
#[derive(Debug, Clone)]
pub struct MediaServiceConfig {
    /// Master switch for lunchboxd's background prefetch.
    pub prefetch: bool,
    /// Whether prefetch continues while an activity is running.
    pub prefetch_while_session_active: bool,
    /// Free-space floor on the cache filesystem, in bytes. 0 disables the
    /// check.
    pub free_space_floor_bytes: u64,
    /// How long, in days, a play protects a cached video from being displaced
    /// by a speculative download.
    pub watched_grace_days: u64,
    /// Maximum total size of the on-disk video cache, in bytes.
    pub cache_max_bytes: u64,
    /// SponsorBlock segment skipping (issue #159).
    pub sponsorblock: SponsorBlockConfig,
}

/// Validated SponsorBlock settings (issue #159).
#[derive(Debug, Clone)]
pub struct SponsorBlockConfig {
    /// Whether to skip segments at all. Off unless a parent turned it on, and
    /// while it is off nothing contacts the service.
    pub enabled: bool,
    /// Category names to skip, already checked against the known list.
    pub categories: Vec<String>,
    /// Base URL of the instance to query.
    pub api: String,
}

impl Default for SponsorBlockConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            categories: crate::schema::DEFAULT_SPONSORBLOCK_CATEGORIES
                .iter()
                .map(|c| c.as_str().to_string())
                .collect(),
            api: "https://sponsor.ajay.app".to_string(),
        }
    }
}

impl Default for MediaServiceConfig {
    fn default() -> Self {
        Self {
            prefetch: true,
            prefetch_while_session_active: false,
            free_space_floor_bytes: 2 * 1024 * 1024 * 1024,
            watched_grace_days: 30,
            cache_max_bytes: 10 * 1024 * 1024 * 1024,
            sponsorblock: SponsorBlockConfig::default(),
        }
    }
}

/// Validated external monitor / docking configuration (issue #87).
#[derive(Debug, Clone)]
pub struct DisplayConfig {
    /// Master switch for docking support.
    pub docking_enabled: bool,
    /// Route audio to the external video device while docked.
    pub mirror_audio: bool,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            docking_enabled: true,
            mirror_audio: true,
        }
    }
}

impl DisplayConfig {
    fn from_raw(raw: Option<&crate::schema::RawDisplayConfig>) -> Self {
        match raw {
            Some(r) => Self {
                docking_enabled: r.docking_enabled,
                mirror_audio: r.mirror_audio,
            },
            None => Self::default(),
        }
    }
}

impl ServiceConfig {
    fn from_raw(raw: RawServiceConfig) -> Self {
        let log_dir = raw.log_dir.clone().unwrap_or_else(default_log_dir);
        let child_log_dir = raw
            .child_log_dir
            .unwrap_or_else(|| log_dir.join("sessions"));
        let internet = convert_internet_config(raw.internet.as_ref());
        let steam = SteamConfig::from_raw(raw.steam.as_ref());
        let management_api = raw
            .management_api
            .as_ref()
            .filter(|c| c.enabled)
            .map(ManagementApiConfig::from_raw);
        let data_dir = raw.data_dir.unwrap_or_else(default_data_dir);
        let ble_management = raw
            .ble_management
            .as_ref()
            .filter(|c| c.enabled)
            .map(BleManagementConfig::from_raw);
        let display = DisplayConfig::from_raw(raw.display.as_ref());
        let file_manager = FileManagerConfig::from_raw(raw.file_manager.as_ref());
        let media = raw
            .media
            .as_ref()
            .map(|m| MediaServiceConfig {
                prefetch: m.prefetch,
                prefetch_while_session_active: m.prefetch_while_session_active,
                free_space_floor_bytes: m.free_space_floor_bytes,
                watched_grace_days: m.watched_grace_days,
                sponsorblock: SponsorBlockConfig {
                    enabled: m.sponsorblock.enabled,
                    categories: m
                        .sponsorblock
                        .categories
                        .iter()
                        .map(|c| c.as_str().to_string())
                        .collect(),
                    api: m.sponsorblock.api.clone(),
                },
                cache_max_bytes: m.cache_max_bytes,
            })
            .unwrap_or_default();
        Self {
            socket_path: raw.socket_path.unwrap_or_else(socket_path_without_env),
            log_dir,
            capture_child_output: raw.capture_child_output,
            child_log_dir,
            data_dir,
            internet,
            steam,
            management_api,
            ble_management,
            display,
            media,
            file_manager,
        }
    }
}

/// Default Steam launch watchdog timeout.
pub const DEFAULT_STEAM_LAUNCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Validated Steam-specific configuration
#[derive(Debug, Clone)]
pub struct SteamConfig {
    /// Known launch interstitials to auto-dismiss via CEF. Empty disables the
    /// feature (and the CEF debug port). Slug strings are validated against
    /// [`InterstitialKind`] by `validate_config` before this is built.
    pub auto_dismiss: HashSet<InterstitialKind>,
    /// How long to wait for a Steam game to appear before ending with an error.
    pub launch_timeout: Duration,
}

impl SteamConfig {
    fn from_raw(raw: Option<&RawSteamConfig>) -> Self {
        let allow_risky = raw.map(|c| c.allow_risky_dismiss).unwrap_or(false);
        let auto_dismiss = match raw.and_then(|c| c.auto_dismiss_interstitials.as_ref()) {
            // Unset → the safe default set.
            None => InterstitialKind::DEFAULT_AUTO_DISMISS.into_iter().collect(),
            // Set (incl. empty) → exactly what's listed. Unknown/risky slugs are
            // rejected by validation, so parse leniently here.
            Some(list) => list
                .iter()
                .filter_map(|s| InterstitialKind::from_slug(s))
                .filter(|k| allow_risky || !k.is_risky())
                .collect(),
        };
        Self {
            auto_dismiss,
            launch_timeout: raw
                .and_then(|c| c.launch_timeout_seconds)
                .map(Duration::from_secs)
                .unwrap_or(DEFAULT_STEAM_LAUNCH_TIMEOUT),
        }
    }
}

impl Default for SteamConfig {
    fn default() -> Self {
        Self {
            auto_dismiss: InterstitialKind::DEFAULT_AUTO_DISMISS.into_iter().collect(),
            launch_timeout: DEFAULT_STEAM_LAUNCH_TIMEOUT,
        }
    }
}

/// Validated management HTTP API configuration
#[derive(Debug, Clone)]
pub struct ManagementApiConfig {
    pub port: u16,
    pub bind: IpAddr,
    /// How long to keep retrying the initial bind when the address is unavailable.
    /// `None` means retry indefinitely.
    pub bind_retry: Option<Duration>,
    /// Machine credential for `Authorization: Bearer`. Since issue #156 it
    /// authenticates a request but cannot open a browser session.
    pub auth_token: Option<String>,
    /// Transport security, with `"auto"` already resolved against `bind`.
    pub tls: TlsMode,
    /// Login and session behaviour.
    pub auth: WebAuthLimits,
}

/// How the management API's listener is secured (issue #156).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TlsMode {
    /// Plaintext. Reachable only on a loopback bind — validation refuses it
    /// anywhere else, so a device cannot be left serving admin in the clear on
    /// a network by leaving a knob at its default.
    Off,
    /// A certificate this device generated for itself, persisted so the
    /// fingerprint a parent accepted stays the same across restarts.
    SelfSigned,
    /// A certificate somebody else issued: `tailscale cert`, Let's Encrypt via
    /// DNS-01, a home CA. The only mode that gets a padlock with no ceremony.
    Files { cert: PathBuf, key: PathBuf },
}

impl TlsMode {
    pub fn is_tls(&self) -> bool {
        !matches!(self, TlsMode::Off)
    }
}

/// Validated login and session behaviour.
#[derive(Debug, Clone)]
pub struct WebAuthLimits {
    pub session_idle: Duration,
    pub session_max_age: Duration,
    pub lockout_after: u32,
    pub lockout: Duration,
}

impl Default for WebAuthLimits {
    fn default() -> Self {
        Self {
            session_idle: DEFAULT_SESSION_IDLE,
            session_max_age: DEFAULT_SESSION_MAX_AGE,
            lockout_after: DEFAULT_LOCKOUT_AFTER,
            lockout: DEFAULT_LOCKOUT,
        }
    }
}

impl ManagementApiConfig {
    fn from_raw(raw: &RawManagementApiConfig) -> Self {
        let bind = raw
            .bind
            .as_deref()
            .and_then(|s| IpAddr::from_str(s).ok())
            .unwrap_or_else(|| IpAddr::from_str(DEFAULT_MANAGEMENT_API_BIND).unwrap());
        let bind_retry = match raw.bind_retry_seconds {
            Some(0) => None,
            Some(s) => Some(Duration::from_secs(s)),
            None => Some(DEFAULT_MANAGEMENT_API_BIND_RETRY),
        };
        let tls = resolve_tls(raw.tls.as_ref(), &bind);
        let auth = raw
            .auth
            .as_ref()
            .map(|a| WebAuthLimits {
                session_idle: a
                    .session_idle_days
                    .map(|d| Duration::from_secs(d * 24 * 3600))
                    .unwrap_or(DEFAULT_SESSION_IDLE),
                session_max_age: a
                    .session_max_days
                    .map(|d| Duration::from_secs(d * 24 * 3600))
                    .unwrap_or(DEFAULT_SESSION_MAX_AGE),
                lockout_after: a.lockout_after.unwrap_or(DEFAULT_LOCKOUT_AFTER),
                lockout: a
                    .lockout_seconds
                    .map(Duration::from_secs)
                    .unwrap_or(DEFAULT_LOCKOUT),
            })
            .unwrap_or_default();
        Self {
            port: raw.port.unwrap_or(DEFAULT_MANAGEMENT_API_PORT),
            bind,
            bind_retry,
            auth_token: raw.auth_token.clone(),
            tls,
            auth,
        }
    }
}

/// Resolve `[service.management_api.tls]` against the bind address.
///
/// `"auto"` — the default, and what a config that says nothing gets — is the
/// whole point of this function: a loopback listener stays plaintext, so the
/// dev loop and the e2e harness are untouched, and a listener anyone else can
/// reach comes up on a self-signed certificate rather than in the clear.
/// A mode string that is not one of the four is rejected in `validate_config`;
/// here it falls back to `"auto"` rather than panicking, because `from_raw`
/// runs on configs that have already been validated and must not have a second
/// opinion about them.
fn resolve_tls(raw: Option<&crate::schema::RawTlsConfig>, bind: &IpAddr) -> TlsMode {
    let Some(raw) = raw else {
        return auto_tls(bind);
    };
    match raw.mode.as_deref().unwrap_or("auto") {
        "off" => TlsMode::Off,
        "self_signed" => TlsMode::SelfSigned,
        "files" => match (raw.cert.as_ref(), raw.key.as_ref()) {
            (Some(cert), Some(key)) => TlsMode::Files {
                cert: PathBuf::from(cert),
                key: PathBuf::from(key),
            },
            // Missing paths are a validation error; a config that reached here
            // without them still has to come up as something, and the
            // self-signed fallback is the one that does not serve admin in the
            // clear.
            _ => TlsMode::SelfSigned,
        },
        _ => auto_tls(bind),
    }
}

fn auto_tls(bind: &IpAddr) -> TlsMode {
    if bind.is_loopback() {
        TlsMode::Off
    } else {
        TlsMode::SelfSigned
    }
}

/// Validated BLE management transport configuration.
#[derive(Debug, Clone)]
pub struct BleManagementConfig {
    pub device_name: String,
    /// Controller address or `hciN` name; `None` means "first listed".
    pub adapter: Option<String>,
}

impl BleManagementConfig {
    fn from_raw(raw: &RawBleManagementConfig) -> Self {
        Self {
            device_name: raw
                .device_name
                .clone()
                .unwrap_or_else(|| "lunchbox".to_string()),
            adapter: raw
                .adapter
                .as_ref()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
        }
    }
}

impl Default for ServiceConfig {
    fn default() -> Self {
        let log_dir = default_log_dir();
        Self {
            socket_path: socket_path_without_env(),
            child_log_dir: log_dir.join("sessions"),
            log_dir,
            data_dir: default_data_dir(),
            capture_child_output: false,
            internet: InternetConfig::new(
                None,
                DEFAULT_INTERNET_CHECK_INTERVAL,
                DEFAULT_INTERNET_CHECK_TIMEOUT,
            ),
            steam: SteamConfig::default(),
            management_api: None,
            ble_management: None,
            display: DisplayConfig::default(),
            media: MediaServiceConfig::default(),
            file_manager: FileManagerConfig::default(),
        }
    }
}

/// Validated entry definition
#[derive(Debug, Clone)]
pub struct Entry {
    pub id: EntryId,
    pub label: String,
    pub icon_ref: Option<String>,
    pub kind: EntryKind,
    pub availability: AvailabilityPolicy,
    pub limits: LimitsPolicy,
    /// Token gate (issue #8). `None` means the entry is not token-gated.
    pub tokens: Option<TokensPolicy>,
    /// Group this entry belongs to (issue #5), whose schedule and limits apply
    /// on top of this entry's own.
    pub group: Option<GroupId>,
    pub warnings: Vec<WarningThreshold>,
    pub volume: Option<VolumePolicy>,
    pub brightness: Option<BrightnessPolicy>,
    pub disabled: bool,
    pub disabled_reason: Option<String>,
    pub internet: EntryInternetPolicy,
    pub firewall: Option<FirewallPolicy>,
    /// Supervised-browser policy, materialized into Chromium managed-policy
    /// JSON + Chrome flags at spawn time.
    pub browser: Option<BrowserPolicy>,
    /// Input compatibility modes — orthogonal sidecars. Deduplicated and
    /// validated (no conflicting gamepad presets) by `Entry::from_raw`.
    pub input_compat: Vec<InputCompatMode>,
    pub input_compat_options: InputCompatOptions,
    /// Physical input device types this entry depends on (issue #96). The
    /// entry is gated (shown as unavailable) whenever any listed type is not
    /// currently connected. Sorted and deduplicated by `Entry::from_raw`; an
    /// empty list means no requirement.
    pub requires_input: Vec<InputDeviceType>,
    /// Drop sway's output scale to 1.0 while this activity is running so
    /// XWayland clients get the panel's native pixel grid. See the
    /// corresponding field on [`RawEntry`] for the full rationale.
    pub xwayland_native_resolution: bool,
    /// Show a confirmation prompt before the HUD "X" button ends this
    /// activity (issue #78). Enabled by default; only affects the "X" button,
    /// not API/expiration/process-exit closes.
    pub confirm_on_close: bool,

    /// Put the HUD on a different screen edge while this activity runs
    /// (issue #171). `None` inherits the global `[service.hud]` setting.
    pub hud_orientation: Option<HudOrientation>,
}

impl Entry {
    /// This entry as a limit subject, for cooldown / token / override lookups.
    pub fn subject(&self) -> LimitSubject {
        LimitSubject::Entry(self.id.clone())
    }

    fn from_raw(
        raw: RawEntry,
        default_warnings: &[WarningThreshold],
        defaults: LimitDefaults,
        _global_volume: &VolumePolicy,
        _global_brightness: &BrightnessPolicy,
    ) -> Self {
        let kind = convert_entry_kind(raw.kind);
        // Resolved before `kind` is moved into the policy below: an entry that
        // says nothing about confirming inherits its kind's answer, and a
        // reader has nothing unsaved to protect.
        let confirm_on_close = raw
            .confirm_on_close
            .unwrap_or_else(|| kind.confirms_on_close_by_default());
        let availability = raw
            .availability
            .map(convert_availability)
            .unwrap_or_default();
        let limits = raw
            .limits
            .map(|l| convert_limits(l, defaults))
            .unwrap_or_else(|| LimitsPolicy {
                max_run: defaults.max_run,
                daily_quota: None, // None means unlimited
                cooldown: None,
                cooldown_min_session: defaults.cooldown_min_session,
                save_grace: defaults.save_grace,
            });
        let tokens = raw.tokens.as_ref().map(convert_tokens);
        let group = raw.group.as_ref().map(GroupId::new);
        let warnings = raw
            .warnings
            .map(|w| w.into_iter().map(convert_warning).collect())
            .unwrap_or_else(|| default_warnings.to_vec());
        let volume = raw.volume.as_ref().map(convert_volume_config);
        let brightness = raw.brightness.as_ref().map(convert_brightness_config);
        let internet = convert_entry_internet(raw.internet.as_ref());
        let firewall = raw.firewall.as_ref().map(convert_firewall_config);
        let browser = raw.browser.as_ref().map(convert_browser_config);
        // Resolved before `kind` is moved into the policy below. An entry that
        // lists nothing inherits its kind's sidecars; `input_compat = []` is a
        // list, so it still means "none" and reaches the same conflict pass.
        let input_compat = match &raw.input_compat {
            Some(listed) => convert_input_compat_list(listed, &EntryId::new(raw.id.clone())),
            None => kind.default_input_compat(),
        };
        let input_compat_options = raw
            .input_compat_options
            .as_ref()
            .map(convert_input_compat_options)
            .unwrap_or_default();
        let requires_input = convert_input_device_list(&raw.requires_input);

        Self {
            id: EntryId::new(raw.id),
            label: raw.label,
            icon_ref: raw.icon.or_else(|| autodetect_icon(&kind)),
            kind,
            availability,
            limits,
            tokens,
            group,
            warnings,
            volume,
            brightness,
            disabled: raw.disabled,
            disabled_reason: raw.disabled_reason,
            internet,
            firewall,
            browser,
            input_compat,
            input_compat_options,
            requires_input,
            xwayland_native_resolution: raw.xwayland_native_resolution,
            confirm_on_close,
            hud_orientation: raw.hud_orientation.map(convert_hud_orientation),
        }
    }
}

/// Map the config spelling of a HUD edge onto the wire type (issue #171).
fn convert_hud_orientation(raw: RawHudOrientation) -> HudOrientation {
    match raw {
        RawHudOrientation::Top => HudOrientation::Top,
        RawHudOrientation::Bottom => HudOrientation::Bottom,
        RawHudOrientation::Left => HudOrientation::Left,
    }
}

/// When an entry is available
#[derive(Debug, Clone, Default)]
pub struct AvailabilityPolicy {
    /// Time windows when entry is available
    pub windows: Vec<TimeWindow>,
    /// If true, always available (ignores windows)
    pub always: bool,
}

impl AvailabilityPolicy {
    /// Check if available at given local time
    pub fn is_available(&self, dt: &chrono::DateTime<chrono::Local>) -> bool {
        if self.always {
            return true;
        }
        if self.windows.is_empty() {
            return true; // No windows = always available
        }
        self.windows.iter().any(|w| w.contains(dt))
    }

    /// Get remaining time in current window
    pub fn remaining_in_window(&self, dt: &chrono::DateTime<chrono::Local>) -> Option<Duration> {
        if self.always {
            return None; // No limit from windows
        }
        self.windows.iter().find_map(|w| w.remaining_duration(dt))
    }
}

/// Token gate for an entry (issue #8)
///
/// Sessions on the entries in `from` bank time on this entry's balance, which
/// this entry's own sessions then spend down. The entry is unavailable while
/// the balance is zero or below `minimum`.
#[derive(Debug, Clone)]
pub struct TokensPolicy {
    /// Subjects whose sessions bank time toward this one — individual entries,
    /// or whole groups (issue #5). Sorted and deduplicated by `convert_tokens`;
    /// validated to be non-empty and to exclude anything that would let the
    /// gated activity unlock itself.
    pub from: Vec<LimitSubject>,
    /// Seconds banked per second spent on a source entry.
    pub earn_ratio: f64,
    /// Balance required before this entry unlocks. `Duration::ZERO` means any
    /// balance above zero unlocks it.
    pub minimum: Duration,
    /// Ceiling on the banked balance. None means unlimited.
    pub max_balance: Option<Duration>,
    /// Whether the balance survives local midnight.
    pub carry_over: bool,
}

impl TokensPolicy {
    /// Whether the gate is open for `balance`.
    ///
    /// `minimum` has to be banked every time the gate opens, not only the
    /// first (issue #193). It is there so that a session is long enough to be
    /// worth starting — a whole battle, a whole level — and a gate that stayed
    /// open below it would hand out exactly the short sessions it exists to
    /// prevent. A session that does start is still capped by the *whole*
    /// balance rather than `balance - minimum`, so nothing is cut off at the
    /// threshold; whatever it leaves below the threshold stays banked and
    /// counts toward opening the gate again.
    pub fn unlocked(&self, balance: Duration) -> bool {
        !balance.is_zero() && balance >= self.minimum
    }

    /// Time banked by a session of `duration` on one of the source entries,
    /// before the `max_balance` ceiling is applied. Truncated to whole
    /// seconds, matching the storage granularity.
    pub fn earned(&self, duration: Duration) -> Duration {
        Duration::from_secs((duration.as_secs() as f64 * self.earn_ratio).floor() as u64)
    }
}

/// Time limits for an entry
#[derive(Debug, Clone)]
pub struct LimitsPolicy {
    /// Maximum run duration. None means unlimited.
    pub max_run: Option<Duration>,
    /// Daily quota. None means unlimited.
    pub daily_quota: Option<Duration>,
    pub cooldown: Option<Duration>,
    /// How long a session has to last before it starts the cooldown. Sessions
    /// shorter than this leave the cooldown untouched, so an activity that
    /// crashes on launch doesn't lock the child out of it (a workaround for
    /// unstable activities). `Duration::ZERO` means every session, however
    /// short, starts the cooldown. Resolved at config load time from
    /// `limits.cooldown_min_session_seconds`, falling back to
    /// `service.cooldown_min_session_seconds`.
    pub cooldown_min_session: Duration,
    /// How long a running session gets to save its progress when a resume from
    /// sleep lands outside its allowed hours (issue #155). `Duration::ZERO`
    /// ends it as soon as the machine wakes.
    ///
    /// Resolved at config load time from `limits.save_grace_seconds`, falling
    /// back to the group's, then to `service.save_grace_seconds`. Unlike the
    /// other limits this genuinely cascades rather than being evaluated at both
    /// levels: a session belongs to one activity, so only the entry's resolved
    /// value is ever read — including when it was the *group's* window that
    /// closed, since the child is saving the activity in front of them either
    /// way.
    pub save_grace: Duration,
}

/// Network firewall policy applied to a session at spawn time.
///
/// Rules are passed verbatim to systemd's `IPAddressAllow=`/`IPAddressDeny=`
/// properties on the per-session scope. Validated at config load time:
/// each rule is either a parseable CIDR or one of the systemd address tokens.
#[derive(Debug, Clone)]
pub struct FirewallPolicy {
    /// If true, deny all traffic by default; only `allow` rules pass.
    /// If false, allow all traffic by default; `deny` rules block.
    pub default_deny: bool,
    /// Allow rules (CIDR strings or systemd address tokens)
    pub allow: Vec<String>,
    /// Deny rules (applied after allow)
    pub deny: Vec<String>,
}

fn convert_firewall_config(raw: &RawFirewallConfig) -> FirewallPolicy {
    let default_deny = !raw.default.eq_ignore_ascii_case("allow");
    // Rules were already validated by `validate_config`; canonicalize whitespace
    // so the strings we hand to systemd are clean.
    let normalize = |rules: &[String]| -> Vec<String> {
        rules
            .iter()
            .map(|r| parse_firewall_rule(r).unwrap_or_else(|_| r.trim().to_string()))
            .collect()
    };
    FirewallPolicy {
        default_deny,
        allow: normalize(&raw.allow),
        deny: normalize(&raw.deny),
    }
}

/// Validated supervised-browser policy applied to an entry at spawn time.
///
/// Fields are validated at config load time by `validate_browser`:
/// `profile_id` is a safe path segment, `mode` is a known window mode,
/// `start_url` is an http(s) URL, and URL patterns are non-empty/whitespace-free.
#[derive(Debug, Clone)]
pub struct BrowserPolicy {
    /// On-disk user-data-dir segment (shared across entries with the same id).
    pub profile_id: String,
    /// How Chrome is launched.
    pub mode: BrowserMode,
    /// URL opened on launch, if any.
    pub start_url: Option<String>,
    /// Chromium `URLAllowlist` patterns.
    pub url_allowlist: Vec<String>,
    /// Chromium `URLBlocklist` patterns, applied after the allowlist.
    pub url_blocklist: Vec<String>,
    /// Disable DevTools.
    pub disable_dev_tools: bool,
    /// Disable incognito mode.
    pub disable_incognito: bool,
    /// Block extension installation.
    pub disable_extensions: bool,
    /// Wipe the profile directory after the session ends.
    pub wipe_on_exit: bool,
}

fn convert_browser_config(raw: &RawBrowserConfig) -> BrowserPolicy {
    // `mode` was validated by `validate_config`; default to kiosk defensively.
    let mode = match raw.mode.trim().to_ascii_lowercase().as_str() {
        "app" => BrowserMode::App,
        "windowed" => BrowserMode::Windowed,
        _ => BrowserMode::Kiosk,
    };
    let normalize = |patterns: &[String]| -> Vec<String> {
        patterns.iter().map(|p| p.trim().to_string()).collect()
    };
    BrowserPolicy {
        profile_id: raw.profile_id.trim().to_string(),
        mode,
        start_url: raw.start_url.as_ref().map(|s| s.trim().to_string()),
        url_allowlist: normalize(&raw.url_allowlist),
        url_blocklist: normalize(&raw.url_blocklist),
        disable_dev_tools: raw.disable_dev_tools,
        disable_incognito: raw.disable_incognito,
        disable_extensions: raw.disable_extensions,
        wipe_on_exit: raw.wipe_on_exit,
    }
}

/// Volume control policy
#[derive(Debug, Clone, Default)]
pub struct VolumePolicy {
    /// Maximum volume percentage allowed (enforced by the service)
    pub max_volume: Option<u8>,
    /// Minimum volume percentage allowed (enforced by the service)
    pub min_volume: Option<u8>,
    /// Whether mute toggle is allowed
    pub allow_mute: bool,
    /// Whether volume changes are allowed at all
    pub allow_change: bool,
}

impl VolumePolicy {
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

/// Screen-brightness control policy
#[derive(Debug, Clone, Default)]
pub struct BrightnessPolicy {
    /// Maximum brightness percentage allowed
    pub max_brightness: Option<u8>,
    /// Minimum brightness percentage allowed
    pub min_brightness: Option<u8>,
    /// Whether brightness changes are allowed at all
    pub allow_change: bool,
}

impl BrightnessPolicy {
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

/// Automatic (ambient-light) brightness policy. Device-global; there is no
/// per-entry auto override. `min_percent`/`max_percent` bound the auto range;
/// the per-entry/global [`BrightnessPolicy`] restrictions clamp on top of that
/// (so a bedtime activity's `max_brightness` still caps auto brightness).
#[derive(Debug, Clone)]
pub struct AutoBrightnessPolicy {
    /// Default enabled state (runtime toggle, once set, overrides this).
    pub enabled: bool,
    /// Lux at or below which brightness sits at `min_percent`.
    pub dim_lux: f32,
    /// Lux at or above which brightness sits at `max_percent`.
    pub bright_lux: f32,
    /// Brightness percent at the dim end of the curve.
    pub min_percent: u8,
    /// Brightness percent at the bright end of the curve.
    pub max_percent: u8,
    /// How often to sample the ambient light sensor.
    pub poll_interval: Duration,
}

impl Default for AutoBrightnessPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            dim_lux: 10.0,
            bright_lux: 1000.0,
            min_percent: 15,
            max_percent: 100,
            poll_interval: Duration::from_secs(3),
        }
    }
}

// Conversion helpers

fn convert_entry_kind(raw: RawEntryKind) -> EntryKind {
    match raw {
        RawEntryKind::Process {
            command,
            args,
            env,
            cwd,
        } => EntryKind::Process {
            command,
            args,
            env,
            cwd,
        },
        RawEntryKind::Snap {
            snap_name,
            command,
            args,
            env,
        } => EntryKind::Snap {
            snap_name,
            command,
            args,
            env,
        },
        RawEntryKind::Steam { app_id, args, env } => EntryKind::Steam { app_id, args, env },
        RawEntryKind::Flatpak { app_id, args, env } => EntryKind::Flatpak { app_id, args, env },
        RawEntryKind::Vm { driver, args } => EntryKind::Vm { driver, args },
        RawEntryKind::Media {
            library,
            mode,
            item,
            quality,
            sort_by,
            reverse,
            resume,
            prefetch,
            sponsorblock,
        } => EntryKind::Media {
            library,
            mode: match mode {
                RawMediaMode::Browse => MediaMode::Browse,
                RawMediaMode::Play => MediaMode::Play,
            },
            item,
            quality: match quality {
                RawMediaQuality::Best => MediaQuality::Best,
                RawMediaQuality::Q1080 => MediaQuality::Q1080,
                RawMediaQuality::Q720 => MediaQuality::Q720,
                RawMediaQuality::Q480 => MediaQuality::Q480,
            },
            sort_by: match sort_by {
                RawMediaSortBy::Library => MediaSortBy::Library,
                RawMediaSortBy::Title => MediaSortBy::Title,
                RawMediaSortBy::Id => MediaSortBy::Id,
                RawMediaSortBy::Kind => MediaSortBy::Kind,
                RawMediaSortBy::Category => MediaSortBy::Category,
                RawMediaSortBy::Duration => MediaSortBy::Duration,
            },
            reverse,
            resume,
            prefetch,
            sponsorblock,
        },
        RawEntryKind::Retroarch {
            core,
            core_path,
            content,
            save_state,
            command,
            args,
            env,
            kiosk,
            reset,
        } => EntryKind::Retroarch {
            core,
            core_path,
            content,
            save_state,
            command,
            args,
            env,
            kiosk,
            reset,
        },
        RawEntryKind::Ebook {
            book,
            viewer,
            open_at,
            layout,
            font_size,
            font_family,
            command,
            args,
            env,
            kiosk,
        } => EntryKind::Ebook {
            book,
            viewer,
            open_at,
            layout,
            font_size,
            font_family,
            command,
            args,
            env,
            kiosk,
        },
        RawEntryKind::Custom { type_name, payload } => EntryKind::Custom {
            type_name,
            payload: payload.unwrap_or(serde_json::Value::Null),
        },
    }
}

fn convert_availability(raw: crate::schema::RawAvailability) -> AvailabilityPolicy {
    let windows = raw.windows.into_iter().map(convert_time_window).collect();
    AvailabilityPolicy {
        windows,
        always: raw.always,
    }
}

fn convert_volume_config(raw: &RawVolumeConfig) -> VolumePolicy {
    VolumePolicy {
        max_volume: raw.max_volume,
        min_volume: raw.min_volume,
        allow_mute: raw.allow_mute,
        allow_change: raw.allow_change,
    }
}

fn convert_brightness_config(raw: &RawBrightnessConfig) -> BrightnessPolicy {
    BrightnessPolicy {
        max_brightness: raw.max_brightness,
        min_brightness: raw.min_brightness,
        allow_change: raw.allow_change,
    }
}

fn convert_auto_brightness_config(raw: &RawAutoBrightnessConfig) -> AutoBrightnessPolicy {
    let defaults = AutoBrightnessPolicy::default();
    AutoBrightnessPolicy {
        enabled: raw.enabled,
        dim_lux: raw.dim_lux.unwrap_or(defaults.dim_lux),
        bright_lux: raw.bright_lux.unwrap_or(defaults.bright_lux),
        min_percent: raw.min_percent.unwrap_or(defaults.min_percent),
        max_percent: raw.max_percent.unwrap_or(defaults.max_percent),
        poll_interval: raw
            .poll_interval_seconds
            .map(Duration::from_secs)
            .unwrap_or(defaults.poll_interval),
    }
}

fn convert_internet_config(raw: Option<&RawInternetConfig>) -> InternetConfig {
    let check = raw
        .and_then(|cfg| cfg.check.as_ref())
        .and_then(|value| InternetCheckTarget::parse(value).ok());

    let interval = raw
        .and_then(|cfg| cfg.interval_seconds)
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_INTERNET_CHECK_INTERVAL);

    let timeout = raw
        .and_then(|cfg| cfg.timeout_ms)
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_INTERNET_CHECK_TIMEOUT);

    InternetConfig::new(check, interval, timeout)
}

fn convert_input_compat(raw: RawInputCompat) -> InputCompatMode {
    match raw {
        RawInputCompat::TouchToMouse => InputCompatMode::TouchToMouse,
        RawInputCompat::TabletToTouch => InputCompatMode::TabletToTouch,
        RawInputCompat::DisableTouch => InputCompatMode::DisableTouch,
        RawInputCompat::GamepadProductivity => InputCompatMode::GamepadProductivity,
        RawInputCompat::GamepadGpd => InputCompatMode::GamepadGpd,
    }
}

/// Convert and validate a list of input-compat modes. Duplicates are dropped;
/// conflicting gamepad presets log a warning and the first one wins. This
/// keeps invalid configs from silently launching two competing sidecars.
fn convert_input_compat_list(raw: &[RawInputCompat], entry_id: &EntryId) -> Vec<InputCompatMode> {
    let mut out: Vec<InputCompatMode> = Vec::new();
    let mut have_gamepad = false;
    for r in raw {
        let mode = convert_input_compat(*r);
        if mode.is_gamepad() {
            if have_gamepad {
                tracing::warn!(
                    entry = %entry_id.as_str(),
                    "Multiple gamepad input_compat presets configured; ignoring extras",
                );
                continue;
            }
            have_gamepad = true;
        }
        // The touch-handling modes (touch_to_mouse, tablet_to_touch,
        // disable_touch) all grab or produce the touchscreen, so stacking two
        // of them would have them fight over the same devices or form a loop
        // (tablet → synthetic touchscreen → mouse). Keep the first-configured
        // one and drop any later conflicting mode.
        if mode.handles_touch() && out.iter().any(|m| m.handles_touch()) {
            tracing::warn!(
                entry = %entry_id.as_str(),
                "input_compat has multiple touch-handling modes (touch_to_mouse, \
                 tablet_to_touch, disable_touch), which are mutually exclusive; \
                 ignoring the later one",
            );
            continue;
        }
        if out.contains(&mode) {
            continue;
        }
        out.push(mode);
    }
    out
}

fn convert_input_compat_options(raw: &RawInputCompatOptions) -> InputCompatOptions {
    InputCompatOptions {
        gamepad_deadzone: raw.gamepad_deadzone,
        gamepad_mouse_speed: raw.gamepad_mouse_speed,
        gamepad_scroll_speed: raw.gamepad_scroll_speed,
    }
}

fn convert_input_device(raw: RawInputDevice) -> InputDeviceType {
    match raw {
        RawInputDevice::Mouse => InputDeviceType::Mouse,
        RawInputDevice::Touch => InputDeviceType::Touch,
        RawInputDevice::Keyboard => InputDeviceType::Keyboard,
        RawInputDevice::Gamepad => InputDeviceType::Gamepad,
    }
}

/// Convert a raw `requires_input` list into a sorted, deduplicated set of
/// device types. Sorting keeps the gating reason (and any UI text derived from
/// it) stable regardless of config order.
fn convert_input_device_list(raw: &[RawInputDevice]) -> Vec<InputDeviceType> {
    let mut out: Vec<InputDeviceType> = raw.iter().copied().map(convert_input_device).collect();
    out.sort();
    out.dedup();
    out
}

fn convert_entry_internet(raw: Option<&crate::schema::RawEntryInternet>) -> EntryInternetPolicy {
    let required = raw.map(|cfg| cfg.required).unwrap_or(false);
    let check = raw
        .and_then(|cfg| cfg.check.as_ref())
        .and_then(|value| InternetCheckTarget::parse(value).ok());
    // Absent `[entries.internet]` means "inherit and forward", same as the
    // block being present with everything left at its default.
    let forward_check = raw.map(|cfg| cfg.forward_check).unwrap_or(true);

    EntryInternetPolicy {
        required,
        check,
        forward_check,
    }
}

fn convert_time_window(raw: crate::schema::RawTimeWindow) -> TimeWindow {
    let days_mask = parse_days(&raw.days).unwrap_or(0x7F);
    let (start_h, start_m) = parse_time(&raw.start).unwrap_or((0, 0));
    let (end_h, end_m) = parse_time(&raw.end).unwrap_or((23, 59));

    TimeWindow {
        days: DaysOfWeek::new(days_mask),
        start: WallClock::new(start_h, start_m).unwrap(),
        end: WallClock::new(end_h, end_m).unwrap(),
    }
}

/// Convert seconds to Duration, treating 0 as "unlimited" (None)
fn seconds_to_duration_or_unlimited(secs: u64) -> Option<Duration> {
    if secs == 0 {
        None // 0 means unlimited
    } else {
        Some(Duration::from_secs(secs))
    }
}

fn convert_limits(raw: crate::schema::RawLimits, defaults: LimitDefaults) -> LimitsPolicy {
    LimitsPolicy {
        max_run: raw
            .max_run_seconds
            .map(seconds_to_duration_or_unlimited)
            .unwrap_or(defaults.max_run),
        daily_quota: raw
            .daily_quota_seconds
            .and_then(seconds_to_duration_or_unlimited),
        cooldown: raw.cooldown_seconds.map(Duration::from_secs),
        cooldown_min_session: raw
            .cooldown_min_session_seconds
            .map(Duration::from_secs)
            .unwrap_or(defaults.cooldown_min_session),
        save_grace: raw
            .save_grace_seconds
            .map(Duration::from_secs)
            .unwrap_or(defaults.save_grace),
    }
}

fn convert_tokens(raw: &crate::schema::RawTokens) -> TokensPolicy {
    let mut from: Vec<String> = raw.from.clone();
    from.sort();
    from.dedup();
    // Parsing is infallible: an unprefixed ID is an entry, `group:` marks a group.
    let from: Vec<LimitSubject> = from
        .into_iter()
        .map(|s| s.parse().expect("LimitSubject parsing is infallible"))
        .collect();

    TokensPolicy {
        from,
        earn_ratio: raw.earn_ratio.unwrap_or(DEFAULT_TOKEN_EARN_RATIO),
        minimum: Duration::from_secs(raw.minimum_seconds.unwrap_or(0)),
        max_balance: raw
            .max_balance_seconds
            .and_then(seconds_to_duration_or_unlimited),
        carry_over: raw.carry_over,
    }
}

fn convert_warning(raw: RawWarningThreshold) -> WarningThreshold {
    let severity = match raw.severity.to_lowercase().as_str() {
        "info" => WarningSeverity::Info,
        "critical" => WarningSeverity::Critical,
        _ => WarningSeverity::Warn,
    };

    WarningThreshold {
        seconds_before: raw.seconds_before,
        severity,
        message_template: raw.message,
    }
}

pub(crate) fn default_warning_thresholds() -> Vec<WarningThreshold> {
    vec![
        WarningThreshold {
            seconds_before: 300, // 5 minutes
            severity: WarningSeverity::Info,
            message_template: Some("5 minutes remaining".into()),
        },
        WarningThreshold {
            seconds_before: 60, // 1 minute
            severity: WarningSeverity::Warn,
            message_template: Some("1 minute remaining".into()),
        },
        WarningThreshold {
            seconds_before: 10,
            severity: WarningSeverity::Critical,
            message_template: Some("10 seconds remaining!".into()),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Local, TimeZone};

    #[test]
    fn test_availability_always() {
        let policy = AvailabilityPolicy {
            windows: vec![],
            always: true,
        };

        let dt = lunchbox_util::now();
        assert!(policy.is_available(&dt));
    }

    #[test]
    fn input_compat_dedups_and_resolves_conflicts() {
        let id = EntryId::new("e");
        // Duplicate touch → one entry. Both gamepad presets → only first kept.
        let out = convert_input_compat_list(
            &[
                RawInputCompat::TouchToMouse,
                RawInputCompat::TouchToMouse,
                RawInputCompat::GamepadProductivity,
                RawInputCompat::GamepadGpd,
            ],
            &id,
        );
        assert_eq!(
            out,
            vec![
                InputCompatMode::TouchToMouse,
                InputCompatMode::GamepadProductivity,
            ]
        );
    }

    #[test]
    fn input_compat_drops_inverse_pointer_touch_pair() {
        let id = EntryId::new("e");
        // touch_to_mouse and tablet_to_touch invert each other; the later one
        // is dropped while a stacked gamepad preset survives.
        let out = convert_input_compat_list(
            &[
                RawInputCompat::TouchToMouse,
                RawInputCompat::TabletToTouch,
                RawInputCompat::GamepadProductivity,
            ],
            &id,
        );
        assert_eq!(
            out,
            vec![
                InputCompatMode::TouchToMouse,
                InputCompatMode::GamepadProductivity,
            ]
        );

        // Order-independent: whichever direction is configured first wins.
        let out = convert_input_compat_list(
            &[RawInputCompat::TabletToTouch, RawInputCompat::TouchToMouse],
            &id,
        );
        assert_eq!(out, vec![InputCompatMode::TabletToTouch]);
    }

    #[test]
    fn input_compat_disable_touch_excludes_other_touch_modes() {
        let id = EntryId::new("e");
        // disable_touch is mutually exclusive with the other touch-handling
        // modes; the first-configured touch mode wins and a stacked gamepad
        // preset survives.
        let out = convert_input_compat_list(
            &[
                RawInputCompat::DisableTouch,
                RawInputCompat::TouchToMouse,
                RawInputCompat::TabletToTouch,
                RawInputCompat::GamepadGpd,
            ],
            &id,
        );
        assert_eq!(
            out,
            vec![InputCompatMode::DisableTouch, InputCompatMode::GamepadGpd]
        );

        // disable_touch stacks fine with a gamepad preset on its own.
        let out = convert_input_compat_list(
            &[
                RawInputCompat::GamepadProductivity,
                RawInputCompat::DisableTouch,
            ],
            &id,
        );
        assert_eq!(
            out,
            vec![
                InputCompatMode::GamepadProductivity,
                InputCompatMode::DisableTouch,
            ]
        );
    }

    #[test]
    fn test_availability_window() {
        let policy = AvailabilityPolicy {
            windows: vec![TimeWindow {
                days: DaysOfWeek::ALL_DAYS,
                start: WallClock::new(14, 0).unwrap(),
                end: WallClock::new(18, 0).unwrap(),
            }],
            always: false,
        };

        // 3 PM should be available
        let dt = Local.with_ymd_and_hms(2025, 12, 26, 15, 0, 0).unwrap();
        assert!(policy.is_available(&dt));

        // 10 AM should not be available
        let dt = Local.with_ymd_and_hms(2025, 12, 26, 10, 0, 0).unwrap();
        assert!(!policy.is_available(&dt));
    }
}
