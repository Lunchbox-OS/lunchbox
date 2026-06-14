//! Configuration validation

use crate::internet::InternetCheckTarget;
use crate::schema::{
    RawBrowserConfig, RawConfig, RawDays, RawEntry, RawEntryKind, RawFirewallConfig, RawTimeWindow,
};
use std::collections::HashSet;
use std::net::IpAddr;
use std::str::FromStr;
use thiserror::Error;

/// Validation error
#[derive(Debug, Clone, Error)]
pub enum ValidationError {
    #[error("Entry '{entry_id}': {message}")]
    EntryError { entry_id: String, message: String },

    #[error("Duplicate entry ID: {0}")]
    DuplicateEntryId(String),

    #[error("Invalid time format '{value}': {message}")]
    InvalidTimeFormat { value: String, message: String },

    #[error("Invalid day specification: {0}")]
    InvalidDaySpec(String),

    #[error("Warning threshold {seconds}s >= max_run {max_run}s for entry '{entry_id}'")]
    WarningExceedsMaxRun {
        entry_id: String,
        seconds: u64,
        max_run: u64,
    },

    #[error("Global config error: {0}")]
    GlobalError(String),
}

/// Validate a raw configuration
pub fn validate_config(config: &RawConfig) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    // Validate global internet check (if set)
    if let Some(internet) = &config.service.internet
        && let Some(check) = &internet.check
        && let Err(e) = InternetCheckTarget::parse(check)
    {
        errors.push(ValidationError::GlobalError(format!(
            "Invalid internet check '{}': {}",
            check, e
        )));
    }

    if let Some(internet) = &config.service.internet {
        if let Some(interval) = internet.interval_seconds
            && interval == 0
        {
            errors.push(ValidationError::GlobalError(
                "Internet check interval_seconds must be > 0".into(),
            ));
        }
        if let Some(timeout) = internet.timeout_ms
            && timeout == 0
        {
            errors.push(ValidationError::GlobalError(
                "Internet check timeout_ms must be > 0".into(),
            ));
        }
    }

    // Validate Steam interstitial auto-dismiss slugs.
    if let Some(steam) = &config.service.steam
        && let Some(list) = &steam.auto_dismiss_interstitials
    {
        for slug in list {
            match shepherd_api::InterstitialKind::from_slug(slug) {
                None => {
                    let known: Vec<&str> = shepherd_api::InterstitialKind::ALL
                        .iter()
                        .map(|k| k.slug())
                        .collect();
                    errors.push(ValidationError::GlobalError(format!(
                        "Unknown steam interstitial '{}' in auto_dismiss_interstitials (known: {})",
                        slug,
                        known.join(", ")
                    )));
                }
                Some(kind) if kind.is_risky() && !steam.allow_risky_dismiss => {
                    errors.push(ValidationError::GlobalError(format!(
                        "Steam interstitial '{}' is risky (launches an unusable game); set \
                         service.steam.allow_risky_dismiss = true to enable it",
                        slug
                    )));
                }
                Some(_) => {}
            }
        }
    }

    // Check for duplicate entry IDs
    let mut seen_ids = HashSet::new();
    for entry in &config.entries {
        if !seen_ids.insert(&entry.id) {
            errors.push(ValidationError::DuplicateEntryId(entry.id.clone()));
        }
    }

    // Validate each entry
    for entry in &config.entries {
        errors.extend(validate_entry(entry, config));
    }

    errors
}

fn validate_entry(entry: &RawEntry, config: &RawConfig) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    // Validate kind
    match &entry.kind {
        RawEntryKind::Process { command, .. } => {
            if command.is_empty() {
                errors.push(ValidationError::EntryError {
                    entry_id: entry.id.clone(),
                    message: "command cannot be empty".into(),
                });
            }
        }
        RawEntryKind::Snap { snap_name, .. } => {
            if snap_name.is_empty() {
                errors.push(ValidationError::EntryError {
                    entry_id: entry.id.clone(),
                    message: "snap_name cannot be empty".into(),
                });
            }
        }
        RawEntryKind::Steam { app_id, .. } => {
            if *app_id == 0 {
                errors.push(ValidationError::EntryError {
                    entry_id: entry.id.clone(),
                    message: "app_id must be > 0".into(),
                });
            }
        }
        RawEntryKind::Flatpak { app_id, .. } => {
            if app_id.is_empty() {
                errors.push(ValidationError::EntryError {
                    entry_id: entry.id.clone(),
                    message: "app_id cannot be empty".into(),
                });
            }
        }
        RawEntryKind::Vm { driver, .. } => {
            if driver.is_empty() {
                errors.push(ValidationError::EntryError {
                    entry_id: entry.id.clone(),
                    message: "VM driver cannot be empty".into(),
                });
            }
        }
        RawEntryKind::Media { library_id, .. } => {
            if library_id.is_empty() {
                errors.push(ValidationError::EntryError {
                    entry_id: entry.id.clone(),
                    message: "library_id cannot be empty".into(),
                });
            }
        }
        RawEntryKind::Custom { type_name, .. } => {
            if type_name.is_empty() {
                errors.push(ValidationError::EntryError {
                    entry_id: entry.id.clone(),
                    message: "type_name cannot be empty".into(),
                });
            }
        }
    }

    // Validate availability windows
    if let Some(avail) = &entry.availability {
        for window in &avail.windows {
            errors.extend(validate_time_window(window, &entry.id));
        }
    }

    // Validate warning thresholds vs max_run
    // Skip validation if max_run is 0 (unlimited) since there's no expiry to warn about
    let max_run = entry
        .limits
        .as_ref()
        .and_then(|l| l.max_run_seconds)
        .or(config.service.default_max_run_seconds);

    // Only validate warnings if max_run is Some and not 0 (unlimited)
    if let (Some(warnings), Some(max_run)) = (&entry.warnings, max_run)
        && max_run > 0
    {
        for warning in warnings {
            if warning.seconds_before >= max_run {
                errors.push(ValidationError::WarningExceedsMaxRun {
                    entry_id: entry.id.clone(),
                    seconds: warning.seconds_before,
                    max_run,
                });
            }
        }
        // Note: warnings are ignored for unlimited entries (max_run = 0)
    }

    // Validate firewall rules
    if let Some(firewall) = &entry.firewall {
        errors.extend(validate_firewall(firewall, &entry.id));
    }

    // Validate browser policy
    if let Some(browser) = &entry.browser {
        errors.extend(validate_browser(browser, &entry.id));
    }

    // Validate internet requirements
    if let Some(internet) = &entry.internet {
        if let Some(check) = &internet.check
            && let Err(e) = InternetCheckTarget::parse(check)
        {
            errors.push(ValidationError::EntryError {
                entry_id: entry.id.clone(),
                message: format!("Invalid internet check '{}': {}", check, e),
            });
        }

        if internet.required {
            let has_check = internet.check.is_some()
                || config
                    .service
                    .internet
                    .as_ref()
                    .and_then(|cfg| cfg.check.as_ref())
                    .is_some();
            if !has_check {
                errors.push(ValidationError::EntryError {
                    entry_id: entry.id.clone(),
                    message: "internet is required but no check is configured (set service.internet.check or entries.internet.check)".into(),
                });
            }
        }
    }

    errors
}

fn validate_time_window(window: &RawTimeWindow, entry_id: &str) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    // Validate days
    if let Err(e) = parse_days(&window.days) {
        errors.push(ValidationError::EntryError {
            entry_id: entry_id.to_string(),
            message: e,
        });
    }

    // Validate start time
    if let Err(e) = parse_time(&window.start) {
        errors.push(ValidationError::InvalidTimeFormat {
            value: window.start.clone(),
            message: e,
        });
    }

    // Validate end time
    if let Err(e) = parse_time(&window.end) {
        errors.push(ValidationError::InvalidTimeFormat {
            value: window.end.clone(),
            message: e,
        });
    }

    errors
}

fn validate_firewall(firewall: &RawFirewallConfig, entry_id: &str) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    let default = firewall.default.to_ascii_lowercase();
    if default != "allow" && default != "deny" {
        errors.push(ValidationError::EntryError {
            entry_id: entry_id.to_string(),
            message: format!(
                "firewall.default must be \"allow\" or \"deny\", got \"{}\"",
                firewall.default
            ),
        });
    }

    for (list_name, rules) in [("allow", &firewall.allow), ("deny", &firewall.deny)] {
        for rule in rules {
            if let Err(e) = parse_firewall_rule(rule) {
                errors.push(ValidationError::EntryError {
                    entry_id: entry_id.to_string(),
                    message: format!("firewall.{} rule \"{}\": {}", list_name, rule, e),
                });
            }
        }
    }

    errors
}

/// Validate and canonicalize a firewall rule.
///
/// Accepts:
/// - systemd address tokens: `any`, `localhost`, `link-local`, `multicast`
/// - bare IPv4 / IPv6 addresses
/// - CIDR ranges (`10.0.0.0/8`, `2001:db8::/32`)
///
/// Returns the canonical (trimmed) string suitable for handing to systemd.
pub fn parse_firewall_rule(rule: &str) -> Result<String, String> {
    let trimmed = rule.trim();
    if trimmed.is_empty() {
        return Err("rule is empty".into());
    }

    // systemd-supported address tokens
    let lower = trimmed.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "any" | "localhost" | "link-local" | "multicast"
    ) {
        return Ok(lower);
    }

    // Split off optional CIDR prefix length
    let (addr_part, prefix_part) = match trimmed.split_once('/') {
        Some((a, p)) => (a, Some(p)),
        None => (trimmed, None),
    };

    let addr = IpAddr::from_str(addr_part)
        .map_err(|_| format!("\"{}\" is not a valid IP address", addr_part))?;

    if let Some(prefix_str) = prefix_part {
        let prefix: u8 = prefix_str
            .parse()
            .map_err(|_| format!("\"{}\" is not a valid prefix length", prefix_str))?;
        let max = if addr.is_ipv4() { 32 } else { 128 };
        if prefix > max {
            return Err(format!(
                "prefix /{} exceeds maximum /{} for {}",
                prefix,
                max,
                if addr.is_ipv4() { "IPv4" } else { "IPv6" }
            ));
        }
    }

    Ok(trimmed.to_string())
}

fn validate_browser(browser: &RawBrowserConfig, entry_id: &str) -> Vec<ValidationError> {
    let mut errors = Vec::new();
    let mut push = |message: String| {
        errors.push(ValidationError::EntryError {
            entry_id: entry_id.to_string(),
            message,
        });
    };

    if let Err(e) = validate_profile_id(&browser.profile_id) {
        push(format!("browser.profile_id {}", e));
    }

    let mode = browser.mode.trim().to_ascii_lowercase();
    if !matches!(mode.as_str(), "kiosk" | "app" | "windowed") {
        push(format!(
            "browser.mode must be \"kiosk\", \"app\", or \"windowed\", got \"{}\"",
            browser.mode
        ));
    }

    if let Some(start_url) = &browser.start_url
        && let Err(e) = validate_http_url(start_url)
    {
        push(format!("browser.start_url \"{}\": {}", start_url, e));
    }

    for (list_name, patterns) in [
        ("url_allowlist", &browser.url_allowlist),
        ("url_blocklist", &browser.url_blocklist),
    ] {
        for pattern in patterns {
            if let Err(e) = validate_url_pattern(pattern) {
                push(format!(
                    "browser.{} pattern \"{}\": {}",
                    list_name, pattern, e
                ));
            }
        }
    }

    errors
}

/// Validate a `profile_id` used as a single on-disk path segment. Rejects
/// anything that could escape the user-data-dir or break path handling.
fn validate_profile_id(id: &str) -> Result<(), String> {
    let trimmed = id.trim();
    if trimmed.is_empty() {
        return Err("cannot be empty".into());
    }
    if trimmed == "." || trimmed == ".." {
        return Err("cannot be \".\" or \"..\"".into());
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err("may only contain ASCII letters, digits, '-', '_', '.'".into());
    }
    Ok(())
}

/// Validate an http(s) start URL. Light scheme/host check only — Chrome is the
/// authority on URL semantics; this just catches obvious config mistakes.
fn validate_http_url(url: &str) -> Result<(), String> {
    let trimmed = url.trim();
    let (scheme, rest) = trimmed
        .split_once("://")
        .ok_or("must be an http:// or https:// URL")?;
    if !matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https") {
        return Err("must be an http:// or https:// URL".into());
    }
    if rest.is_empty() || rest.starts_with('/') {
        return Err("missing host".into());
    }
    Ok(())
}

/// Validate a Chromium URL-filter pattern. Full pattern semantics are left to
/// Chrome; we only reject empty/whitespace-bearing values that can never match.
fn validate_url_pattern(pattern: &str) -> Result<(), String> {
    let trimmed = pattern.trim();
    if trimmed.is_empty() {
        return Err("is empty".into());
    }
    if trimmed.chars().any(char::is_whitespace) {
        return Err("must not contain whitespace".into());
    }
    Ok(())
}

/// Parse HH:MM time format
pub fn parse_time(s: &str) -> Result<(u8, u8), String> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 2 {
        return Err("Expected HH:MM format".into());
    }

    let hour: u8 = parts[0].parse().map_err(|_| "Invalid hour".to_string())?;
    let minute: u8 = parts[1].parse().map_err(|_| "Invalid minute".to_string())?;

    if hour >= 24 {
        return Err("Hour must be 0-23".into());
    }
    if minute >= 60 {
        return Err("Minute must be 0-59".into());
    }

    Ok((hour, minute))
}

/// Parse days specification
pub fn parse_days(days: &RawDays) -> Result<u8, String> {
    match days {
        RawDays::Preset(preset) => match preset.to_lowercase().as_str() {
            "all" | "every" | "daily" => Ok(0x7F),
            "weekdays" => Ok(0x1F), // Mon-Fri
            "weekends" => Ok(0x60), // Sat-Sun
            other => Err(format!("Unknown day preset: {}", other)),
        },
        RawDays::List(list) => {
            let mut mask = 0u8;
            for day in list {
                let bit = match day.to_lowercase().as_str() {
                    "mon" | "monday" => 1 << 0,
                    "tue" | "tuesday" => 1 << 1,
                    "wed" | "wednesday" => 1 << 2,
                    "thu" | "thursday" => 1 << 3,
                    "fri" | "friday" => 1 << 4,
                    "sat" | "saturday" => 1 << 5,
                    "sun" | "sunday" => 1 << 6,
                    other => return Err(format!("Unknown day: {}", other)),
                };
                mask |= bit;
            }
            Ok(mask)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_time() {
        assert_eq!(parse_time("14:30").unwrap(), (14, 30));
        assert_eq!(parse_time("00:00").unwrap(), (0, 0));
        assert_eq!(parse_time("23:59").unwrap(), (23, 59));

        assert!(parse_time("24:00").is_err());
        assert!(parse_time("12:60").is_err());
        assert!(parse_time("invalid").is_err());
    }

    #[test]
    fn test_parse_days() {
        assert_eq!(
            parse_days(&RawDays::Preset("weekdays".into())).unwrap(),
            0x1F
        );
        assert_eq!(
            parse_days(&RawDays::Preset("weekends".into())).unwrap(),
            0x60
        );
        assert_eq!(parse_days(&RawDays::Preset("all".into())).unwrap(), 0x7F);

        assert_eq!(
            parse_days(&RawDays::List(vec![
                "mon".into(),
                "wed".into(),
                "fri".into()
            ]))
            .unwrap(),
            0b10101
        );
    }

    #[test]
    fn test_parse_firewall_rule_accepts_tokens() {
        assert_eq!(parse_firewall_rule("any").unwrap(), "any");
        assert_eq!(parse_firewall_rule("LOCALHOST").unwrap(), "localhost");
        assert_eq!(parse_firewall_rule("link-local").unwrap(), "link-local");
        assert_eq!(parse_firewall_rule("multicast").unwrap(), "multicast");
    }

    #[test]
    fn test_parse_firewall_rule_accepts_addresses() {
        assert_eq!(parse_firewall_rule("10.0.0.1").unwrap(), "10.0.0.1");
        assert_eq!(parse_firewall_rule("10.0.0.0/8").unwrap(), "10.0.0.0/8");
        assert_eq!(parse_firewall_rule("::1").unwrap(), "::1");
        assert_eq!(
            parse_firewall_rule("2001:db8::/32").unwrap(),
            "2001:db8::/32"
        );
        // Whitespace is trimmed
        assert_eq!(
            parse_firewall_rule("  192.168.1.0/24  ").unwrap(),
            "192.168.1.0/24"
        );
    }

    #[test]
    fn test_parse_firewall_rule_rejects_garbage() {
        assert!(parse_firewall_rule("").is_err());
        assert!(parse_firewall_rule("notanip").is_err());
        assert!(parse_firewall_rule("10.0.0.1/abc").is_err());
        assert!(parse_firewall_rule("10.0.0.1/33").is_err());
        assert!(parse_firewall_rule("::1/129").is_err());
        assert!(parse_firewall_rule("example.com").is_err());
    }

    #[test]
    fn test_validate_firewall_default_must_be_known() {
        let cfg = RawFirewallConfig {
            default: "maybe".into(),
            allow: vec![],
            deny: vec![],
        };
        let errors = validate_firewall(&cfg, "x");
        assert!(errors.iter().any(|e| matches!(
            e,
            ValidationError::EntryError { message, .. } if message.contains("default")
        )));
    }

    #[test]
    fn test_validate_firewall_propagates_rule_errors() {
        let cfg = RawFirewallConfig {
            default: "deny".into(),
            allow: vec!["10.0.0.0/8".into(), "garbage".into()],
            deny: vec![],
        };
        let errors = validate_firewall(&cfg, "x");
        assert_eq!(errors.len(), 1);
    }

    fn browser(profile_id: &str, mode: &str) -> RawBrowserConfig {
        RawBrowserConfig {
            profile_id: profile_id.into(),
            mode: mode.into(),
            start_url: None,
            url_allowlist: vec![],
            url_blocklist: vec![],
            disable_dev_tools: true,
            disable_incognito: true,
            disable_extensions: true,
            wipe_on_exit: false,
        }
    }

    #[test]
    fn test_validate_browser_accepts_valid() {
        let mut cfg = browser("school", "kiosk");
        cfg.start_url = Some("https://classroom.google.com".into());
        cfg.url_allowlist = vec!["https://*.google.com/*".into()];
        assert!(validate_browser(&cfg, "x").is_empty());
        assert!(validate_browser(&browser("a_b-c.1", "app"), "x").is_empty());
        assert!(validate_browser(&browser("p", "windowed"), "x").is_empty());
    }

    #[test]
    fn test_validate_browser_rejects_bad_profile_id() {
        for bad in ["", "..", ".", "a/b", "../etc", "has space", "a\\b"] {
            let errors = validate_browser(&browser(bad, "kiosk"), "x");
            assert!(
                errors
                    .iter()
                    .any(|e| matches!(e, ValidationError::EntryError { message, .. } if message.contains("profile_id"))),
                "expected profile_id error for {bad:?}"
            );
        }
    }

    #[test]
    fn test_validate_browser_rejects_bad_mode() {
        let errors = validate_browser(&browser("school", "fullscreen"), "x");
        assert!(errors.iter().any(|e| matches!(
            e,
            ValidationError::EntryError { message, .. } if message.contains("mode")
        )));
    }

    #[test]
    fn test_validate_browser_rejects_bad_start_url() {
        for bad in [
            "ftp://x",
            "classroom.google.com",
            "https://",
            "javascript:alert(1)",
        ] {
            let mut cfg = browser("school", "kiosk");
            cfg.start_url = Some(bad.into());
            let errors = validate_browser(&cfg, "x");
            assert!(
                errors
                    .iter()
                    .any(|e| matches!(e, ValidationError::EntryError { message, .. } if message.contains("start_url"))),
                "expected start_url error for {bad:?}"
            );
        }
    }

    #[test]
    fn test_validate_browser_rejects_bad_url_pattern() {
        let mut cfg = browser("school", "kiosk");
        cfg.url_allowlist = vec!["https://ok.com/*".into(), "bad pattern".into()];
        cfg.url_blocklist = vec!["".into()];
        let errors = validate_browser(&cfg, "x");
        assert_eq!(errors.len(), 2);
    }

    #[test]
    fn test_duplicate_id_detection() {
        let config = RawConfig {
            config_version: 1,
            service: Default::default(),
            entries: vec![
                RawEntry {
                    id: "game".into(),
                    label: "Game 1".into(),
                    icon: None,
                    kind: RawEntryKind::Process {
                        command: "game1".into(),
                        args: vec![],
                        env: Default::default(),
                        cwd: None,
                    },
                    availability: None,
                    limits: None,
                    warnings: None,
                    volume: None,
                    brightness: None,
                    disabled: false,
                    disabled_reason: None,
                    internet: None,
                    firewall: None,
                    browser: None,
                    input_compat: vec![],
                    input_compat_options: None,
                    xwayland_native_resolution: false,
                },
                RawEntry {
                    id: "game".into(),
                    label: "Game 2".into(),
                    icon: None,
                    kind: RawEntryKind::Process {
                        command: "game2".into(),
                        args: vec![],
                        env: Default::default(),
                        cwd: None,
                    },
                    availability: None,
                    limits: None,
                    warnings: None,
                    volume: None,
                    brightness: None,
                    disabled: false,
                    disabled_reason: None,
                    internet: None,
                    firewall: None,
                    browser: None,
                    input_compat: vec![],
                    input_compat_options: None,
                    xwayland_native_resolution: false,
                },
            ],
        };

        let errors = validate_config(&config);
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, ValidationError::DuplicateEntryId(_)))
        );
    }
}
