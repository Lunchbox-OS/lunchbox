//! Validated policy structures

use crate::icon::autodetect_icon;
use crate::internet::{
    DEFAULT_INTERNET_CHECK_INTERVAL, DEFAULT_INTERNET_CHECK_TIMEOUT, EntryInternetPolicy,
    InternetCheckTarget, InternetConfig,
};
use crate::schema::{
    RawBrightnessConfig, RawBrowserConfig, RawConfig, RawEntry, RawEntryKind, RawFirewallConfig,
    RawInputCompat, RawInputCompatOptions, RawInternetConfig, RawManagementApiConfig,
    RawServiceConfig, RawSteamConfig, RawVolumeConfig, RawWarningThreshold,
};
use crate::validation::{parse_days, parse_firewall_rule, parse_time};
use shepherd_api::{
    BrowserMode, EntryKind, InputCompatMode, InputCompatOptions, InterstitialKind, WarningSeverity,
    WarningThreshold,
};
use shepherd_util::{
    DaysOfWeek, EntryId, TimeWindow, WallClock, default_data_dir, default_log_dir,
    socket_path_without_env,
};
use std::collections::HashSet;
use std::net::IpAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

/// Validated policy ready for use by the core engine
#[derive(Debug, Clone)]
pub struct Policy {
    /// Service configuration
    pub service: ServiceConfig,

    /// Validated entries
    pub entries: Vec<Entry>,

    /// Default warning thresholds
    pub default_warnings: Vec<WarningThreshold>,

    /// Default max run duration. None means unlimited.
    pub default_max_run: Option<Duration>,

    /// Global volume restrictions
    pub volume: VolumePolicy,

    /// Global screen-brightness restrictions
    pub brightness: BrightnessPolicy,
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

        // 0 means unlimited, None means use 1 hour default
        let default_max_run = raw
            .service
            .default_max_run_seconds
            .map(seconds_to_duration_or_unlimited)
            .unwrap_or(Some(Duration::from_secs(3600))); // 1 hour default

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

        let entries = raw
            .entries
            .into_iter()
            .map(|e| {
                Entry::from_raw(
                    e,
                    &default_warnings,
                    default_max_run,
                    &global_volume,
                    &global_brightness,
                )
            })
            .collect();

        Self {
            service: ServiceConfig::from_raw(raw.service),
            entries,
            default_warnings,
            default_max_run,
            volume: global_volume,
            brightness: global_brightness,
        }
    }

    /// Get entry by ID
    pub fn get_entry(&self, id: &EntryId) -> Option<&Entry> {
        self.entries.iter().find(|e| &e.id == id)
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
        Self {
            socket_path: raw.socket_path.unwrap_or_else(socket_path_without_env),
            log_dir,
            capture_child_output: raw.capture_child_output,
            child_log_dir,
            data_dir: raw.data_dir.unwrap_or_else(default_data_dir),
            internet,
            steam,
            management_api,
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
    pub auth_token: Option<String>,
}

impl ManagementApiConfig {
    fn from_raw(raw: &RawManagementApiConfig) -> Self {
        let bind = raw
            .bind
            .as_deref()
            .and_then(|s| IpAddr::from_str(s).ok())
            .unwrap_or_else(|| IpAddr::from_str("127.0.0.1").unwrap());
        let bind_retry = match raw.bind_retry_seconds {
            Some(0) => None,
            Some(s) => Some(Duration::from_secs(s)),
            None => Some(Duration::from_secs(300)),
        };
        Self {
            port: raw.port.unwrap_or(7890),
            bind,
            bind_retry,
            auth_token: raw.auth_token.clone(),
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
    /// Drop sway's output scale to 1.0 while this activity is running so
    /// XWayland clients get the panel's native pixel grid. See the
    /// corresponding field on [`RawEntry`] for the full rationale.
    pub xwayland_native_resolution: bool,
    /// Show a confirmation prompt before the HUD "X" button ends this
    /// activity (issue #78). Enabled by default; only affects the "X" button,
    /// not API/expiration/process-exit closes.
    pub confirm_on_close: bool,
}

impl Entry {
    fn from_raw(
        raw: RawEntry,
        default_warnings: &[WarningThreshold],
        default_max_run: Option<Duration>,
        _global_volume: &VolumePolicy,
        _global_brightness: &BrightnessPolicy,
    ) -> Self {
        let kind = convert_entry_kind(raw.kind);
        let availability = raw
            .availability
            .map(convert_availability)
            .unwrap_or_default();
        let limits = raw
            .limits
            .map(|l| convert_limits(l, default_max_run))
            .unwrap_or_else(|| LimitsPolicy {
                max_run: default_max_run,
                daily_quota: None, // None means unlimited
                cooldown: None,
            });
        let warnings = raw
            .warnings
            .map(|w| w.into_iter().map(convert_warning).collect())
            .unwrap_or_else(|| default_warnings.to_vec());
        let volume = raw.volume.as_ref().map(convert_volume_config);
        let brightness = raw.brightness.as_ref().map(convert_brightness_config);
        let internet = convert_entry_internet(raw.internet.as_ref());
        let firewall = raw.firewall.as_ref().map(convert_firewall_config);
        let browser = raw.browser.as_ref().map(convert_browser_config);
        let input_compat =
            convert_input_compat_list(&raw.input_compat, &EntryId::new(raw.id.clone()));
        let input_compat_options = raw
            .input_compat_options
            .as_ref()
            .map(convert_input_compat_options)
            .unwrap_or_default();

        Self {
            id: EntryId::new(raw.id),
            label: raw.label,
            icon_ref: raw.icon.or_else(|| autodetect_icon(&kind)),
            kind,
            availability,
            limits,
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
            xwayland_native_resolution: raw.xwayland_native_resolution,
            confirm_on_close: raw.confirm_on_close,
        }
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

/// Time limits for an entry
#[derive(Debug, Clone)]
pub struct LimitsPolicy {
    /// Maximum run duration. None means unlimited.
    pub max_run: Option<Duration>,
    /// Daily quota. None means unlimited.
    pub daily_quota: Option<Duration>,
    pub cooldown: Option<Duration>,
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
        RawEntryKind::Media { library_id, args } => EntryKind::Media { library_id, args },
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

fn convert_entry_internet(raw: Option<&crate::schema::RawEntryInternet>) -> EntryInternetPolicy {
    let required = raw.map(|cfg| cfg.required).unwrap_or(false);
    let check = raw
        .and_then(|cfg| cfg.check.as_ref())
        .and_then(|value| InternetCheckTarget::parse(value).ok());

    EntryInternetPolicy { required, check }
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

fn convert_limits(
    raw: crate::schema::RawLimits,
    default_max_run: Option<Duration>,
) -> LimitsPolicy {
    LimitsPolicy {
        max_run: raw
            .max_run_seconds
            .map(seconds_to_duration_or_unlimited)
            .unwrap_or(default_max_run),
        daily_quota: raw
            .daily_quota_seconds
            .and_then(seconds_to_duration_or_unlimited),
        cooldown: raw.cooldown_seconds.map(Duration::from_secs),
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

fn default_warning_thresholds() -> Vec<WarningThreshold> {
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

        let dt = shepherd_util::now();
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
