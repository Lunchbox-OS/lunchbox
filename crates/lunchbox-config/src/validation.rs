//! Configuration validation

use crate::internet::InternetCheckTarget;
use crate::policy::{DEFAULT_SESSION_IDLE, DEFAULT_SESSION_MAX_AGE};
use crate::schema::{
    RawBrowserConfig, RawConfig, RawDays, RawEntry, RawEntryKind, RawFirewallConfig, RawGroup,
    RawManagementApiConfig, RawMediaMode, RawTimeWindow, RawTokens,
};
use lunchbox_util::GROUP_SUBJECT_PREFIX;
use std::collections::HashSet;
use std::net::IpAddr;
use std::path::Path;
use std::str::FromStr;
use thiserror::Error;

/// Validation error
#[derive(Debug, Clone, Error)]
pub enum ValidationError {
    #[error("Entry '{entry_id}': {message}")]
    EntryError { entry_id: String, message: String },

    #[error("Duplicate entry ID: {0}")]
    DuplicateEntryId(String),

    #[error("Group '{group_id}': {message}")]
    GroupError { group_id: String, message: String },

    #[error("Duplicate group ID: {0}")]
    DuplicateGroupId(String),

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

/// Directories the file manager's extra roots may neither be nor contain
/// (issue #195).
///
/// Not a security boundary — everything here is already unreadable to the uid
/// lunchboxd runs as, so a root pointing at `/etc` would list it and fail to
/// open most of it. It is a typo guard, and it is where the mistake gets
/// caught: `path = "/"` in a config file is a plausible slip and a browsable
/// root filesystem is not a thing to discover later.
const FORBIDDEN_FILE_MANAGER_ROOTS: &[&str] = &[
    "/",
    "/boot",
    "/dev",
    "/etc",
    "/proc",
    "/root",
    "/sys",
    "/var/lib/lunchboxd",
];

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

    // Validate SponsorBlock settings (issue #159). Only reachable when a
    // parent turned the feature on — an unread `categories` list under
    // `enabled = false` is not worth an error.
    if let Some(media) = &config.service.media
        && media.sponsorblock.enabled
    {
        let sb = &media.sponsorblock;
        if sb.categories.is_empty() {
            errors.push(ValidationError::GlobalError(
                "service.media.sponsorblock is enabled with no categories, so nothing \
                 would be skipped; list categories or set enabled = false"
                    .into(),
            ));
        }
        // An unknown category name never reaches here: `RawSponsorBlockCategory`
        // is an enum, so the parse refuses it and names the alternatives.
        if let Err(e) = validate_http_url(&sb.api) {
            errors.push(ValidationError::GlobalError(format!(
                "Invalid service.media.sponsorblock.api '{}': {e}",
                sb.api
            )));
        }
    }

    // Validate the remote file manager's extra roots (issue #195).
    //
    // At parse time rather than at first use: a root that resolves to `/` is
    // a web interface that browses the whole device, and the moment to find
    // that out is while the administrator is still looking at the file they
    // just wrote.
    if let Some(fm) = &config.service.file_manager {
        let mut seen: HashSet<&str> = HashSet::new();
        for root in &fm.extra_roots {
            let label = root.label.trim();
            if label.is_empty() {
                errors.push(ValidationError::GlobalError(
                    "service.file_manager.extra_roots entry has an empty label".into(),
                ));
            } else if !seen.insert(label) {
                errors.push(ValidationError::GlobalError(format!(
                    "service.file_manager.extra_roots has two roots labelled '{label}'"
                )));
            }
            if !root.path.is_absolute() {
                errors.push(ValidationError::GlobalError(format!(
                    "service.file_manager.extra_roots path '{}' must be absolute",
                    root.path.display()
                )));
                continue;
            }
            // Refused if the root *is* a protected directory or *contains*
            // one. Containment is the case that matters: `/var` is a perfectly
            // ordinary-looking path that hands out `/var/lib/lunchboxd`, where
            // the policy, the admin record and the web credentials live.
            if let Some(bad) = FORBIDDEN_FILE_MANAGER_ROOTS.iter().find(|f| {
                let forbidden = std::path::Path::new(f);
                root.path == forbidden || forbidden.starts_with(&root.path)
            }) {
                errors.push(ValidationError::GlobalError(format!(
                    "service.file_manager.extra_roots path '{}' is or contains '{bad}', which \
                     would put system files on the web interface",
                    root.path.display()
                )));
            }
        }
    }

    // Validate automatic-brightness settings (if set).
    if let Some(brightness) = &config.service.brightness
        && let Some(auto) = &brightness.auto
    {
        for (name, pct) in [
            ("min_percent", auto.min_percent),
            ("max_percent", auto.max_percent),
        ] {
            if let Some(pct) = pct
                && pct > 100
            {
                errors.push(ValidationError::GlobalError(format!(
                    "brightness.auto.{name} must be 0-100, got {pct}"
                )));
            }
        }
        if let (Some(min), Some(max)) = (auto.min_percent, auto.max_percent)
            && min > max
        {
            errors.push(ValidationError::GlobalError(format!(
                "brightness.auto.min_percent ({min}) must not exceed max_percent ({max})"
            )));
        }
        for (name, lux) in [("dim_lux", auto.dim_lux), ("bright_lux", auto.bright_lux)] {
            if let Some(lux) = lux
                && (!lux.is_finite() || lux < 0.0)
            {
                errors.push(ValidationError::GlobalError(format!(
                    "brightness.auto.{name} must be a non-negative number, got {lux}"
                )));
            }
        }
        if let (Some(dim), Some(bright)) = (auto.dim_lux, auto.bright_lux)
            && dim >= bright
        {
            errors.push(ValidationError::GlobalError(format!(
                "brightness.auto.dim_lux ({dim}) must be less than bright_lux ({bright})"
            )));
        }
        if auto.poll_interval_seconds == Some(0) {
            errors.push(ValidationError::GlobalError(
                "brightness.auto.poll_interval_seconds must be > 0".into(),
            ));
        }
    }

    // Validate the management API's transport security (issue #156).
    if let Some(api) = &config.service.management_api
        && api.enabled
    {
        errors.extend(validate_management_api(api));
    }

    // Validate Steam interstitial auto-dismiss slugs.
    if let Some(steam) = &config.service.steam
        && let Some(list) = &steam.auto_dismiss_interstitials
    {
        for slug in list {
            match lunchbox_api::InterstitialKind::from_slug(slug) {
                None => {
                    let known: Vec<&str> = lunchbox_api::InterstitialKind::ALL
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

    // Check for duplicate group IDs (issue #5)
    let mut seen_groups = HashSet::new();
    for group in &config.groups {
        if !seen_groups.insert(&group.id) {
            errors.push(ValidationError::DuplicateGroupId(group.id.clone()));
        }
    }

    // Validate each entry
    for entry in &config.entries {
        errors.extend(validate_entry(entry, config));
    }

    // Validate each group
    for group in &config.groups {
        errors.extend(validate_group(group, config));
    }

    errors
}

/// Validate a group definition (issue #5).
/// The management API's own rules.
///
/// The load-bearing one is the last: a listener anyone but this machine can
/// reach may not be in the clear. Everything an administrator does over that
/// socket — a password, a session cookie, a token — is a credential to the
/// child on the same Wi-Fi otherwise. It is an error rather than a warning
/// because a warning is a thing a device ships with.
fn validate_management_api(api: &RawManagementApiConfig) -> Vec<ValidationError> {
    let mut errors = Vec::new();

    let bind = api
        .bind
        .as_deref()
        .unwrap_or(crate::policy::DEFAULT_MANAGEMENT_API_BIND);
    let parsed = match IpAddr::from_str(bind) {
        Ok(ip) => Some(ip),
        Err(_) => {
            errors.push(ValidationError::GlobalError(format!(
                "Invalid service.management_api.bind '{bind}': not an IP address"
            )));
            None
        }
    };

    let mode = api
        .tls
        .as_ref()
        .and_then(|t| t.mode.as_deref())
        .unwrap_or("auto");
    if !matches!(mode, "auto" | "off" | "self_signed" | "files") {
        errors.push(ValidationError::GlobalError(format!(
            "Unknown service.management_api.tls.mode '{mode}' \
             (known: auto, off, self_signed, files)"
        )));
    }

    if mode == "files" {
        let tls = api.tls.as_ref().expect("mode came from tls");
        for (field, value) in [("cert", &tls.cert), ("key", &tls.key)] {
            match value {
                None => errors.push(ValidationError::GlobalError(format!(
                    "service.management_api.tls.mode = \"files\" needs tls.{field}"
                ))),
                Some(path) if !Path::new(path).exists() => {
                    errors.push(ValidationError::GlobalError(format!(
                        "service.management_api.tls.{field} '{path}' does not exist"
                    )))
                }
                Some(_) => {}
            }
        }
    } else if let Some(tls) = &api.tls
        && (tls.cert.is_some() || tls.key.is_some())
    {
        errors.push(ValidationError::GlobalError(format!(
            "service.management_api.tls has cert/key but mode = \"{mode}\"; \
             set mode = \"files\" to use them"
        )));
    }

    if mode == "off"
        && let Some(ip) = parsed
        && !ip.is_loopback()
    {
        errors.push(ValidationError::GlobalError(format!(
            "service.management_api binds {bind} with tls.mode = \"off\", which serves \
             administration in the clear to everyone on that network — including the \
             child this device manages. Use mode = \"self_signed\" (or \"files\" with a \
             certificate), or bind 127.0.0.1. Leaving tls unset picks the right one."
        )));
    }

    if let Some(auth) = &api.auth {
        if auth.session_idle_days == Some(0) {
            errors.push(ValidationError::GlobalError(
                "service.management_api.auth.session_idle_days must be > 0".into(),
            ));
        }
        if auth.session_max_days == Some(0) {
            errors.push(ValidationError::GlobalError(
                "service.management_api.auth.session_max_days must be > 0".into(),
            ));
        }
        if auth.lockout_after == Some(0) {
            errors.push(ValidationError::GlobalError(
                "service.management_api.auth.lockout_after must be > 0; a device that locks \
                 out on zero failures cannot be logged into at all"
                    .into(),
            ));
        }
        // Compared as *resolved* values, not only when both are written down:
        // setting one knob and leaving the other at its default is the ordinary
        // way to get an idle timeout that can never fire, and it used to pass.
        let idle = auth
            .session_idle_days
            .unwrap_or(DEFAULT_SESSION_IDLE.as_secs() / 86_400);
        let max = auth
            .session_max_days
            .unwrap_or(DEFAULT_SESSION_MAX_AGE.as_secs() / 86_400);
        if idle > 0 && max > 0 && idle > max {
            let source = |written: Option<u64>| match written {
                Some(_) => "",
                None => " (the default)",
            };
            errors.push(ValidationError::GlobalError(format!(
                "service.management_api.auth.session_idle_days ({idle}{}) exceeds \
                 session_max_days ({max}{}), so the idle timeout can never fire",
                source(auth.session_idle_days),
                source(auth.session_max_days),
            )));
        }
    }

    errors
}

fn validate_group(group: &RawGroup, config: &RawConfig) -> Vec<ValidationError> {
    let mut errors = Vec::new();
    let err = |message: String| ValidationError::GroupError {
        group_id: group.id.clone(),
        message,
    };

    if group.id.is_empty() {
        errors.push(err("group id cannot be empty".into()));
    }

    // The subject key reserves this prefix to tell groups from entries.
    if group.id.starts_with(GROUP_SUBJECT_PREFIX) {
        errors.push(err(format!(
            "group id cannot start with '{GROUP_SUBJECT_PREFIX}' (the prefix is reserved)"
        )));
    }

    if let Some(availability) = &group.availability {
        for window in &availability.windows {
            errors.extend(validate_time_window(window, &group.id));
        }
    }

    if let Some(tokens) = &group.tokens {
        errors.extend(validate_tokens(
            tokens,
            TokenGateOwner::Group(&group.id),
            config,
        ));
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
        RawEntryKind::Media {
            library,
            mode,
            item,
            ..
        } => {
            if library.trim().is_empty() {
                errors.push(ValidationError::EntryError {
                    entry_id: entry.id.clone(),
                    message: "media library cannot be empty".into(),
                });
            }
            // The item id is what `mode = "play"` plays; without it there is
            // nothing to launch, and with `mode = "browse"` it is silently
            // ignored — both are worth catching here rather than on the
            // child's screen.
            match mode {
                RawMediaMode::Play => {
                    if item.as_ref().is_none_or(|i| i.trim().is_empty()) {
                        errors.push(ValidationError::EntryError {
                            entry_id: entry.id.clone(),
                            message: "media mode = \"play\" requires an item".into(),
                        });
                    }
                }
                RawMediaMode::Browse => {
                    if item.is_some() {
                        errors.push(ValidationError::EntryError {
                            entry_id: entry.id.clone(),
                            message: "media item is only valid with mode = \"play\"".into(),
                        });
                    }
                }
            }
        }
        RawEntryKind::Retroarch {
            core,
            core_path,
            content,
            command,
            ..
        } => {
            errors.extend(validate_retroarch(
                &entry.id,
                core.as_deref(),
                core_path.as_deref(),
                content,
                command,
            ));
        }
        RawEntryKind::Ebook {
            book,
            command,
            font_size,
            font_family,
            open_at,
            ..
        } => {
            errors.extend(validate_ebook(
                &entry.id,
                book,
                command.as_deref(),
                *font_size,
                font_family,
                *open_at,
            ));
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

    // The subject key reserves this prefix to tell groups from entries.
    if entry.id.starts_with(GROUP_SUBJECT_PREFIX) {
        errors.push(ValidationError::EntryError {
            entry_id: entry.id.clone(),
            message: format!(
                "entry id cannot start with '{GROUP_SUBJECT_PREFIX}' (the prefix is reserved \
                 for groups)"
            ),
        });
    }

    // Validate group membership (issue #5)
    if let Some(group) = &entry.group
        && !config.groups.iter().any(|g| &g.id == group)
    {
        errors.push(ValidationError::EntryError {
            entry_id: entry.id.clone(),
            message: format!("group '{group}' is not defined"),
        });
    }

    // Validate the token gate (issue #8)
    if let Some(tokens) = &entry.tokens {
        errors.extend(validate_tokens(
            tokens,
            TokenGateOwner::Entry(entry),
            config,
        ));
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

        // A firewall on a Steam entry is accepted and then quietly ignored:
        // the adapter has no way to apply one to a Steam-launched process,
        // whatever the host supports. Rejecting it here is the only honest
        // answer — an admin who writes it believes the activity is filtered
        // and it is not. It cannot be a runtime diagnostic either, because
        // blocking the launch (issue #143) would remove the activity
        // permanently for a mistake no host change could ever fix.
        if matches!(entry.kind, RawEntryKind::Steam { .. }) {
            errors.push(ValidationError::EntryError {
                entry_id: entry.id.clone(),
                message: "[entries.firewall] is not supported for steam entries; the filter                           cannot be applied to a Steam-launched process. Remove it, or use a                           process/flatpak entry."
                    .into(),
            });
        }
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

/// What a token gate is attached to — an entry or a whole group (issue #5).
#[derive(Clone, Copy)]
enum TokenGateOwner<'a> {
    Entry(&'a RawEntry),
    Group(&'a str),
}

impl TokenGateOwner<'_> {
    fn error(&self, message: String) -> ValidationError {
        match self {
            Self::Entry(entry) => ValidationError::EntryError {
                entry_id: entry.id.clone(),
                message,
            },
            Self::Group(id) => ValidationError::GroupError {
                group_id: (*id).to_string(),
                message,
            },
        }
    }

    /// The TOML table the gate was written in, for error messages.
    fn table(&self) -> &'static str {
        match self {
            Self::Entry(_) => "[entries.tokens]",
            Self::Group(_) => "[groups.tokens]",
        }
    }
}

/// Validate a token gate (issue #8), on either an entry or a group (issue #5).
///
/// This is the one rule that has to resolve IDs against the rest of the config,
/// which is why the validators are handed the whole `RawConfig`.
fn validate_tokens(
    tokens: &RawTokens,
    owner: TokenGateOwner<'_>,
    config: &RawConfig,
) -> Vec<ValidationError> {
    let mut errors = Vec::new();
    let err = |message: String| owner.error(message);

    if tokens.from.is_empty() {
        errors.push(err(format!(
            "tokens.from cannot be empty; remove {} if it is not gated",
            owner.table()
        )));
    }

    for source in &tokens.from {
        match source.strip_prefix(GROUP_SUBJECT_PREFIX) {
            // A group source: every member's time counts toward this gate.
            Some(group_id) => {
                if !config.groups.iter().any(|g| g.id == group_id) {
                    errors.push(err(format!(
                        "tokens.from references unknown group '{group_id}'"
                    )));
                    continue;
                }
                match owner {
                    // A group cannot be unlocked by its own members' time.
                    TokenGateOwner::Group(id) if id == group_id => {
                        errors.push(err(
                            "tokens.from cannot list the group itself; a category cannot \
                             unlock itself"
                                .into(),
                        ));
                    }
                    // Nor can an entry be unlocked by time spent on itself via
                    // the group it belongs to.
                    TokenGateOwner::Entry(entry) if entry.group.as_deref() == Some(group_id) => {
                        errors.push(err(format!(
                            "tokens.from cannot list group '{group_id}', which this entry \
                             belongs to; an activity cannot unlock itself"
                        )));
                    }
                    _ => {}
                }
            }
            // An entry source.
            None => {
                if !config.entries.iter().any(|e| &e.id == source) {
                    errors.push(err(format!(
                        "tokens.from references unknown entry '{source}'"
                    )));
                    continue;
                }
                match owner {
                    TokenGateOwner::Entry(entry) if &entry.id == source => {
                        errors.push(err(
                            "tokens.from cannot list the entry itself; an activity cannot \
                             unlock itself"
                                .into(),
                        ));
                    }
                    // A member's time must not unlock the group gating it.
                    TokenGateOwner::Group(group_id) => {
                        let belongs = config
                            .entries
                            .iter()
                            .any(|e| &e.id == source && e.group.as_deref() == Some(group_id));
                        if belongs {
                            errors.push(err(format!(
                                "tokens.from cannot list '{source}', which is a member of this \
                                 group; a category cannot unlock itself"
                            )));
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    if let Some(ratio) = tokens.earn_ratio
        && (!ratio.is_finite() || ratio <= 0.0)
    {
        errors.push(err(format!(
            "tokens.earn_ratio must be a positive finite number, got {ratio}"
        )));
    }

    // A minimum above the ceiling can never be reached, so the entry would be
    // permanently unavailable.
    if let (Some(minimum), Some(max)) = (tokens.minimum_seconds, tokens.max_balance_seconds)
        && max > 0
        && minimum > max
    {
        errors.push(err(format!(
            "tokens.minimum_seconds ({minimum}) exceeds max_balance_seconds ({max}), so the \
             entry could never unlock"
        )));
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

/// Validate a `type = "retroarch"` entry kind.
///
/// The content path is the strict one: a bare relative path would be resolved
/// against lunchboxd's working directory, not the operator's, so it silently
/// fails to find the ROM at launch time rather than here.
/// Check an `ebook` entry (issue #160).
///
/// The book path gets the same treatment as RetroArch's `content`: a bare
/// relative path would resolve against the daemon's working directory, which is
/// never what an admin means. The font is checked because it is the one setting
/// that cannot be fixed later without moving every remembered reading position.
fn validate_ebook(
    entry_id: &str,
    book: &Path,
    command: Option<&str>,
    font_size: u32,
    font_family: &str,
    open_at: Option<u32>,
) -> Vec<ValidationError> {
    let mut errors = Vec::new();
    let mut err = |message: String| {
        errors.push(ValidationError::EntryError {
            entry_id: entry_id.to_string(),
            message,
        })
    };

    if book.as_os_str().is_empty() {
        err("book cannot be empty".into());
    } else if !is_rooted(book) {
        err(format!(
            "book must be absolute or start with ~/ (a relative path resolves \
             against the daemon's working directory, not yours): {}",
            book.display()
        ));
    }

    if let Some(command) = command
        && command.is_empty()
    {
        err("command cannot be empty".into());
    }

    // Okular renders a reflowed book at this size and paginates from it, so a
    // silly value is not a cosmetic problem: it decides how much of the book
    // fits on a page.
    if !(6..=72).contains(&font_size) {
        err(format!(
            "font_size must be between 6 and 72 points (got {font_size})"
        ));
    }

    if font_family.trim().is_empty() {
        err("font_family cannot be empty".into());
    }

    if open_at == Some(0) {
        err("open_at is a 1-based page number; use 1 for the first page".into());
    }

    errors
}

fn validate_retroarch(
    entry_id: &str,
    core: Option<&str>,
    core_path: Option<&Path>,
    content: &Path,
    command: &str,
) -> Vec<ValidationError> {
    let mut errors = Vec::new();
    let mut err = |message: String| {
        errors.push(ValidationError::EntryError {
            entry_id: entry_id.to_string(),
            message,
        })
    };

    match (core, core_path) {
        (Some(_), Some(_)) => err("set either core or core_path, not both".into()),
        (None, None) => err("one of core (e.g. core = \"mgba\") or core_path is required".into()),
        (Some(name), None) => {
            if name.is_empty() {
                err("core cannot be empty".into());
            } else if name.contains('/') {
                err(format!(
                    "core is a name, not a path: use core = \"{}\" or set core_path instead",
                    Path::new(name)
                        .file_stem()
                        .map(|s| s
                            .to_string_lossy()
                            .trim_end_matches("_libretro")
                            .to_string())
                        .unwrap_or_default()
                ));
            }
        }
        (None, Some(path)) => {
            if !is_rooted(path) {
                err(format!(
                    "core_path must be absolute or start with ~/: {}",
                    path.display()
                ));
            }
        }
    }

    if content.as_os_str().is_empty() {
        err("content cannot be empty".into());
    } else if !is_rooted(content) {
        err(format!(
            "content must be absolute or start with ~/ (a relative path resolves \
             against the daemon's working directory, not yours): {}",
            content.display()
        ));
    }

    if command.is_empty() {
        err("command cannot be empty".into());
    }

    errors
}

/// Whether a configured path is anchored somewhere predictable — absolute, or
/// tilde-relative to the user's home (which the host adapter expands).
fn is_rooted(path: &Path) -> bool {
    path.is_absolute() || path.starts_with("~")
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
    // The catch-all "*" (match every URL) is the one valid wildcard form.
    if trimmed == "*" {
        return Ok(());
    }

    // Chrome's URL-filter format is `[scheme://][.]host[:port][/path][@query]`
    // (https://chromeenterprise.google/policies/url-blocking/). The host has no
    // "*.subdomain" wildcard, and the path is matched as a literal *prefix*, not
    // a glob. Patterns like "https://*.google.com/*" therefore match nothing and
    // get silently swallowed by the authoritative catch-all blocklist — the most
    // common way to misconfigure this list, so reject those forms up front.

    // Drop an optional scheme so the checks below see only host[:port][/path].
    let after_scheme = trimmed.split_once("://").map_or(trimmed, |(_, rest)| rest);
    // Drop an optional leading '.' (the exact-host marker), then isolate the
    // host: it ends at the first port ':' , path '/', or query '@'.
    let host_and_rest = after_scheme.strip_prefix('.').unwrap_or(after_scheme);
    let host_end = host_and_rest
        .find(['/', ':', '@'])
        .unwrap_or(host_and_rest.len());
    let host = &host_and_rest[..host_end];

    // A wildcard is only allowed as the *entire* host; a subdomain wildcard like
    // "*.example.com" is unsupported (and a plain host already covers subdomains).
    if host != "*" && host.contains('*') {
        return Err(format!(
            "host \"{host}\" uses an unsupported wildcard — Chrome has no \"*.host\" form; \
             use the plain host (e.g. \"example.com\"), which already matches all its subdomains"
        ));
    }

    // The path (from the first '/' up to an optional '@query') is a literal
    // prefix, so a '*' there — typically a trailing "/*" — matches nothing.
    if let Some(slash) = host_and_rest[host_end..].find('/') {
        let rest = &host_and_rest[host_end + slash..];
        let path = rest.split('@').next().unwrap_or(rest);
        if path.contains('*') {
            return Err(format!(
                "path \"{path}\" contains \"*\", but Chrome matches the path as a literal prefix, \
                 not a glob; drop it (e.g. use \"example.com/dir\", not \"example.com/dir/*\")"
            ));
        }
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
        // A plain host already matches all subdomains; "*" is the catch-all.
        cfg.url_allowlist = vec![
            "https://google.com".into(),
            "https://accounts.youtube.com".into(),
            ".exact.example.com".into(),
            "https://host.com:8443/path".into(),
            "*".into(),
            "https://*".into(),
        ];
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
        cfg.url_allowlist = vec!["https://ok.com".into(), "bad pattern".into()];
        cfg.url_blocklist = vec!["".into()];
        let errors = validate_browser(&cfg, "x");
        assert_eq!(errors.len(), 2);
    }

    #[test]
    fn test_validate_browser_rejects_unsupported_url_wildcards() {
        // Subdomain-wildcard hosts and path globs are the common Chrome
        // URL-filter mistakes: they match nothing and get swallowed by the
        // catch-all blocklist, so validation must reject them at load time.
        for bad in [
            "https://*.google.com/*",
            "https://*.google.com",
            "*.google.com",
            "https://example.com/dir/*",
            "example.com/*",
        ] {
            let mut cfg = browser("school", "kiosk");
            cfg.url_allowlist = vec![bad.into()];
            let errors = validate_browser(&cfg, "x");
            assert!(
                errors
                    .iter()
                    .any(|e| matches!(e, ValidationError::EntryError { message, .. } if message.contains("url_allowlist"))),
                "expected url_allowlist error for {bad:?}"
            );
        }
    }

    fn firewalled_entry(kind: RawEntryKind) -> RawConfig {
        RawConfig {
            config_version: 1,
            service: Default::default(),
            groups: vec![],
            entries: vec![RawEntry {
                id: "thing".into(),
                label: "Thing".into(),
                icon: None,
                kind,
                availability: None,
                limits: None,
                warnings: None,
                volume: None,
                brightness: None,
                disabled: false,
                disabled_reason: None,
                internet: None,
                firewall: Some(RawFirewallConfig {
                    default: "deny".into(),
                    allow: vec![],
                    deny: vec![],
                }),
                browser: None,
                input_compat: None,
                input_compat_options: None,
                requires_input: vec![],
                tokens: None,
                group: None,
                xwayland_native_resolution: false,
                confirm_on_close: None,
                hud_orientation: None,
            }],
        }
    }

    /// A firewall on a Steam entry is silently ignored at runtime, so an admin
    /// who writes one believes the activity is filtered when it is not. It has
    /// to be caught here: it is a static property of the config, and blocking
    /// the launch instead (issue #143) would remove the activity permanently
    /// for a mistake no host change could fix.
    #[test]
    fn a_firewall_on_a_steam_entry_is_rejected() {
        let config = firewalled_entry(RawEntryKind::Steam {
            app_id: 504230,
            args: vec![],
            env: Default::default(),
        });
        let errors = validate_config(&config);
        assert!(
            errors.iter().any(|e| matches!(
                e,
                ValidationError::EntryError { entry_id, message }
                    if entry_id == "thing" && message.contains("not supported for steam")
            )),
            "expected a steam-firewall rejection, got: {errors:?}"
        );
    }

    /// The kinds that can actually be firewalled must stay accepted, or this
    /// check would break the working configurations it is meant to protect.
    #[test]
    fn a_firewall_on_a_supported_kind_is_accepted() {
        for kind in [
            RawEntryKind::Process {
                command: "browser".into(),
                args: vec![],
                env: Default::default(),
                cwd: None,
            },
            RawEntryKind::Flatpak {
                app_id: "com.google.Chrome".into(),
                args: vec![],
                env: Default::default(),
            },
        ] {
            let errors = validate_config(&firewalled_entry(kind));
            assert!(
                errors.is_empty(),
                "a firewallable kind must validate, got: {errors:?}"
            );
        }
    }

    #[test]
    fn test_duplicate_id_detection() {
        let config = RawConfig {
            config_version: 1,
            service: Default::default(),
            groups: vec![],
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
                    input_compat: None,
                    input_compat_options: None,
                    requires_input: vec![],
                    tokens: None,
                    group: None,
                    xwayland_native_resolution: false,
                    confirm_on_close: None,
                    hud_orientation: None,
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
                    input_compat: None,
                    input_compat_options: None,
                    requires_input: vec![],
                    tokens: None,
                    group: None,
                    xwayland_native_resolution: false,
                    confirm_on_close: None,
                    hud_orientation: None,
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

    /// Validate one `type = "retroarch"` entry whose kind body is `kind_toml`.
    fn retroarch_errors(kind_toml: &str) -> Vec<String> {
        let toml = format!(
            r#"
            config_version = 1

            [[entries]]
            id = "pokemon-firered"
            label = "Pokemon FireRed"
            [entries.kind]
            type = "retroarch"
            {kind_toml}
            "#
        );
        let config: RawConfig = toml::from_str(&toml).expect("test config should parse");
        validate_config(&config)
            .iter()
            .map(|e| e.to_string())
            .collect()
    }

    #[test]
    fn valid_retroarch_entry_passes() {
        assert!(
            retroarch_errors(
                r#"core = "mgba"
                   content = "~/Games/retroarch/pokemon-firered.gba""#
            )
            .is_empty()
        );
    }

    /// A bare relative path resolves against the daemon's working directory,
    /// not the operator's, so it fails at launch time with a confusing error
    /// instead of here. This is the shape of the entry in issue #125.
    #[test]
    fn retroarch_rejects_a_relative_content_path() {
        let errors = retroarch_errors(
            r#"core = "mgba"
               content = "Games/retroarch/pokemon-firered.gba""#,
        );
        assert!(
            errors.iter().any(|e| e.contains("must be absolute")),
            "expected a relative-path error, got: {errors:?}"
        );
    }

    #[test]
    fn retroarch_requires_exactly_one_core_source() {
        let neither = retroarch_errors(r#"content = "/roms/g.gba""#);
        assert!(
            neither.iter().any(|e| e.contains("is required")),
            "expected a missing-core error, got: {neither:?}"
        );

        let both = retroarch_errors(
            r#"core = "mgba"
               core_path = "/opt/cores/mgba_libretro.so"
               content = "/roms/g.gba""#,
        );
        assert!(
            both.iter().any(|e| e.contains("not both")),
            "expected a both-set error, got: {both:?}"
        );
    }

    /// `core` names a core, `core_path` points at one. Writing a path into
    /// `core` would be silently turned into a nonsense filename.
    #[test]
    fn retroarch_rejects_a_path_in_the_core_name() {
        let errors = retroarch_errors(
            r#"core = "/usr/lib/libretro/mgba_libretro.so"
               content = "/roms/g.gba""#,
        );
        assert!(
            errors
                .iter()
                .any(|e| e.contains("core is a name, not a path")),
            "expected a core-name error, got: {errors:?}"
        );
    }

    #[test]
    fn retroarch_rejects_a_relative_core_path() {
        let errors = retroarch_errors(
            r#"core_path = "cores/mgba_libretro.so"
               content = "/roms/g.gba""#,
        );
        assert!(
            errors
                .iter()
                .any(|e| e.contains("core_path must be absolute")),
            "expected a relative core_path error, got: {errors:?}"
        );
    }

    /// Build a two-entry config where "minecraft" is gated on "scratch", with
    /// `tokens_toml` supplying the gate body.
    fn config_with_token_gate(tokens_toml: &str) -> RawConfig {
        let toml = format!(
            r#"
            config_version = 1

            [[entries]]
            id = "scratch"
            label = "Scratch"
            [entries.kind]
            type = "process"
            command = "scratch"

            [[entries]]
            id = "minecraft"
            label = "Minecraft"
            [entries.kind]
            type = "process"
            command = "minecraft"
            [entries.tokens]
            {tokens_toml}
            "#
        );
        toml::from_str(&toml).expect("test config should parse")
    }

    fn token_errors(tokens_toml: &str) -> Vec<String> {
        validate_config(&config_with_token_gate(tokens_toml))
            .iter()
            .map(|e| e.to_string())
            .collect()
    }

    #[test]
    fn valid_token_gate_passes() {
        assert!(
            token_errors(
                r#"from = ["scratch"]
                   earn_ratio = 0.5
                   minimum_seconds = 1800
                   max_balance_seconds = 3600"#
            )
            .is_empty()
        );
    }

    #[test]
    fn token_gate_rejects_unknown_source() {
        let errors = token_errors(r#"from = ["scratch", "nonexistent"]"#);
        assert!(
            errors.iter().any(|e| e.contains("unknown entry")),
            "expected an unknown-entry error, got: {errors:?}"
        );
    }

    #[test]
    fn token_gate_rejects_self_reference() {
        let errors = token_errors(r#"from = ["minecraft"]"#);
        assert!(
            errors.iter().any(|e| e.contains("cannot list the entry")),
            "expected a self-reference error, got: {errors:?}"
        );
    }

    #[test]
    fn token_gate_rejects_empty_source_list() {
        let errors = token_errors("from = []");
        assert!(
            errors.iter().any(|e| e.contains("cannot be empty")),
            "expected an empty-list error, got: {errors:?}"
        );
    }

    #[test]
    fn token_gate_rejects_nonpositive_earn_ratio() {
        for ratio in ["0.0", "-1.0", "nan"] {
            let errors = token_errors(&format!(
                r#"from = ["scratch"]
                   earn_ratio = {ratio}"#
            ));
            assert!(
                errors.iter().any(|e| e.contains("earn_ratio")),
                "expected an earn_ratio error for {ratio}, got: {errors:?}"
            );
        }
    }

    #[test]
    fn token_gate_rejects_unreachable_minimum() {
        // A minimum above the ceiling could never be banked, so the entry would
        // be permanently unavailable.
        let errors = token_errors(
            r#"from = ["scratch"]
               minimum_seconds = 7200
               max_balance_seconds = 3600"#,
        );
        assert!(
            errors.iter().any(|e| e.contains("could never unlock")),
            "expected an unreachable-minimum error, got: {errors:?}"
        );

        // An unlimited ceiling (0) is not a conflict.
        assert!(
            token_errors(
                r#"from = ["scratch"]
                   minimum_seconds = 7200
                   max_balance_seconds = 0"#
            )
            .is_empty()
        );
    }

    /// A config with one group ("games"), a member, and a non-member.
    fn config_with_group(extra: &str) -> RawConfig {
        let toml = format!(
            r#"
            config_version = 1

            [[groups]]
            id = "games"
            label = "Games"

            [[entries]]
            id = "member"
            label = "Member"
            group = "games"
            [entries.kind]
            type = "process"
            command = "member"

            [[entries]]
            id = "outsider"
            label = "Outsider"
            [entries.kind]
            type = "process"
            command = "outsider"

            {extra}
            "#
        );
        toml::from_str(&toml).expect("test config should parse")
    }

    fn group_errors(extra: &str) -> Vec<String> {
        validate_config(&config_with_group(extra))
            .iter()
            .map(|e| e.to_string())
            .collect()
    }

    #[test]
    fn valid_group_config_passes() {
        assert!(group_errors("").is_empty());
    }

    #[test]
    fn entry_referencing_unknown_group_is_rejected() {
        let errors = group_errors(
            r#"[[entries]]
               id = "stray"
               label = "Stray"
               group = "nope"
               [entries.kind]
               type = "process"
               command = "stray""#,
        );
        assert!(
            errors
                .iter()
                .any(|e| e.contains("group 'nope' is not defined")),
            "expected an unknown-group error, got: {errors:?}"
        );
    }

    #[test]
    fn duplicate_group_ids_are_rejected() {
        let errors = group_errors(
            r#"[[groups]]
               id = "games"
               label = "Games Again""#,
        );
        assert!(
            errors.iter().any(|e| e.contains("Duplicate group ID")),
            "expected a duplicate-group error, got: {errors:?}"
        );
    }

    #[test]
    fn reserved_group_prefix_is_rejected_on_ids() {
        // The subject key uses this prefix to tell groups from entries, so
        // neither kind of ID may start with it.
        let errors = group_errors(
            r#"[[entries]]
               id = "group:sneaky"
               label = "Sneaky"
               [entries.kind]
               type = "process"
               command = "sneaky"

               [[groups]]
               id = "group:nested"
               label = "Nested""#,
        );
        assert_eq!(
            errors
                .iter()
                .filter(|e| e.contains("prefix is reserved"))
                .count(),
            2,
            "both the entry and the group ID should be rejected, got: {errors:?}"
        );
    }

    #[test]
    fn token_gate_resolves_group_sources() {
        // A group source is legal...
        assert!(
            group_errors(
                r#"[entries.tokens]
                   from = ["group:games"]"#
            )
            .is_empty(),
            "an entry gated on a whole category should validate"
        );

        // ...but must exist.
        let errors = group_errors(
            r#"[entries.tokens]
               from = ["group:missing"]"#,
        );
        assert!(
            errors.iter().any(|e| e.contains("unknown group 'missing'")),
            "expected an unknown-group source error, got: {errors:?}"
        );
    }

    #[test]
    fn token_gate_rejects_self_unlocking_through_a_group() {
        // A member gated on the group it belongs to would unlock itself.
        let errors = group_errors(
            r#"[[entries]]
               id = "member-2"
               label = "Member 2"
               group = "games"
               [entries.kind]
               type = "process"
               command = "member-2"
               [entries.tokens]
               from = ["group:games"]"#,
        );
        assert!(
            errors
                .iter()
                .any(|e| e.contains("which this entry belongs to")),
            "expected a self-unlock error, got: {errors:?}"
        );
    }

    #[test]
    fn group_token_gate_rejects_self_and_member_sources() {
        // A category cannot be unlocked by itself...
        let errors = group_errors(
            r#"[[groups]]
               id = "other"
               label = "Other"
               [groups.tokens]
               from = ["group:other"]"#,
        );
        assert!(
            errors
                .iter()
                .any(|e| e.contains("cannot list the group itself")),
            "expected a group self-reference error, got: {errors:?}"
        );

        // ...nor by the time its own members spend.
        let errors = group_errors(
            r#"[groups.tokens]
               from = ["member"]"#,
        );
        assert!(
            errors.iter().any(|e| e.contains("member of this group")),
            "expected a member-source error, got: {errors:?}"
        );
    }

    // --- SponsorBlock (issue #159) ---

    fn sponsorblock_errors(table: &str) -> Vec<String> {
        let toml = format!("config_version = 1\n\n[service.media.sponsorblock]\n{table}\n");
        let config: RawConfig = toml::from_str(&toml).expect("test config parses");
        validate_config(&config)
            .iter()
            .map(|e| e.to_string())
            .collect()
    }

    /// The default: off, and therefore nothing to check. Settings nobody is
    /// acting on are not worth an error — an empty category list and an api
    /// that is not a URL are both faults only once the feature is on.
    ///
    /// (A category *name* is checked either way, because the parse refuses an
    /// unknown one before validation sees the file at all.)
    #[test]
    fn sponsorblock_is_not_validated_while_it_is_off() {
        assert!(
            sponsorblock_errors("enabled = false\ncategories = []\napi = \"nonsense\"").is_empty()
        );
    }

    #[test]
    fn sponsorblock_defaults_validate() {
        assert!(sponsorblock_errors("enabled = true").is_empty());
    }

    /// A typo must not quietly stop skipping the category a parent asked for.
    /// It is refused by the parse now rather than by validation, and the error
    /// names the alternatives, which is what the hand-rolled check used to do.
    #[test]
    fn an_unknown_sponsorblock_category_is_refused_by_the_parse() {
        let toml =
            "config_version = 1\n\n[service.media.sponsorblock]\ncategories = [\"sponsors\"]\n";
        let err = toml::from_str::<RawConfig>(toml).expect_err("a typo must not parse");
        let message = err.to_string();
        assert!(message.contains("sponsors"), "{message}");
        assert!(message.contains("selfpromo"), "{message}");
    }

    /// The service's marker categories are real names, and refusing them
    /// alongside the typos is the honest answer: neither describes a span to
    /// jump over.
    #[test]
    fn a_marker_category_is_refused() {
        let toml = "config_version = 1\n\n[service.media.sponsorblock]\ncategories = [\"poi_highlight\"]\n";
        assert!(
            toml::from_str::<RawConfig>(toml).is_err(),
            "markers are not skippable"
        );
    }

    #[test]
    fn sponsorblock_enabled_with_no_categories_is_rejected() {
        let errors = sponsorblock_errors("enabled = true\ncategories = []");
        assert!(
            errors
                .iter()
                .any(|e| e.contains("nothing would be skipped")),
            "{errors:?}"
        );
    }

    #[test]
    fn a_sponsorblock_api_must_be_an_http_url() {
        let errors = sponsorblock_errors("enabled = true\napi = \"sponsor.ajay.app\"");
        assert!(errors.iter().any(|e| e.contains("api")), "{errors:?}");
        assert!(sponsorblock_errors("enabled = true\napi = \"http://sb.lan:8080\"").is_empty());
    }

    // --- media kind (issue #127) ---

    fn media_config(kind_block: &str) -> RawConfig {
        let toml = format!(
            r#"
            config_version = 1

            [[entries]]
            id = "movies"
            label = "Movies"
            [entries.kind]
            {kind_block}
            "#
        );
        toml::from_str(&toml).expect("test config should parse")
    }

    fn media_errors(kind_block: &str) -> Vec<String> {
        validate_config(&media_config(kind_block))
            .iter()
            .map(|e| e.to_string())
            .collect()
    }

    #[test]
    fn media_browse_needs_only_a_library() {
        assert!(
            media_errors("type = \"media\"\nlibrary = \"/etc/shepherd/movies.toml\"").is_empty()
        );
    }

    #[test]
    fn media_accepts_a_playlist_url_as_the_library() {
        let errors = media_errors(
            "type = \"media\"\nlibrary = \"https://www.youtube.com/playlist?list=PL1\"",
        );
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn media_library_cannot_be_blank() {
        let errors = media_errors("type = \"media\"\nlibrary = \"   \"");
        assert!(
            errors.iter().any(|e| e.contains("library cannot be empty")),
            "{errors:?}"
        );
    }

    #[test]
    fn media_play_requires_an_item() {
        let errors = media_errors("type = \"media\"\nlibrary = \"/l.toml\"\nmode = \"play\"");
        assert!(
            errors.iter().any(|e| e.contains("requires an item")),
            "{errors:?}"
        );
    }

    #[test]
    fn media_browse_rejects_an_item() {
        let errors =
            media_errors("type = \"media\"\nlibrary = \"/l.toml\"\nitem = \"big-buck-bunny\"");
        assert!(
            errors
                .iter()
                .any(|e| e.contains("only valid with mode = \"play\"")),
            "{errors:?}"
        );
    }

    #[test]
    fn media_play_with_an_item_passes() {
        let errors = media_errors(
            "type = \"media\"\nlibrary = \"/l.toml\"\nmode = \"play\"\nitem = \"bbb\"",
        );
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn media_rejects_an_unknown_quality_at_parse_time() {
        // serde, not `validate_config`: a typo in an enum field is a parse
        // error, so it can never reach the launch path as a silent default.
        let toml = r#"
            config_version = 1
            [[entries]]
            id = "movies"
            label = "Movies"
            [entries.kind]
            type = "media"
            library = "/l.toml"
            quality = "4k"
        "#;
        assert!(toml::from_str::<RawConfig>(toml).is_err());
    }

    #[test]
    fn entry_internet_forwards_its_check_by_default() {
        let toml = r#"
            config_version = 1
            [[entries]]
            id = "movies"
            label = "Movies"
            [entries.kind]
            type = "media"
            library = "/l.toml"
            [entries.internet]
            check = "https://example.com"
        "#;
        let cfg: RawConfig = toml::from_str(toml).expect("parses");
        assert!(cfg.entries[0].internet.as_ref().unwrap().forward_check);
    }

    #[test]
    fn entry_internet_can_opt_out_of_forwarding() {
        let toml = r#"
            config_version = 1
            [[entries]]
            id = "movies"
            label = "Movies"
            [entries.kind]
            type = "media"
            library = "/l.toml"
            [entries.internet]
            check = "https://example.com"
            forward_check = false
        "#;
        let cfg: RawConfig = toml::from_str(toml).expect("parses");
        assert!(!cfg.entries[0].internet.as_ref().unwrap().forward_check);
    }

    // ---- management API transport security (issue #156) ------------------

    fn api_errors(body: &str) -> Vec<String> {
        let toml =
            format!("config_version = 1\n[service.management_api]\nenabled = true\n{body}\n");
        let cfg: RawConfig = toml::from_str(&toml).expect("parses");
        validate_config(&cfg)
            .into_iter()
            .map(|e| e.to_string())
            .collect()
    }

    #[test]
    fn plaintext_on_a_public_bind_is_refused() {
        let errors = api_errors("bind = \"0.0.0.0\"\n[service.management_api.tls]\nmode = \"off\"");
        assert!(
            errors.iter().any(|e| e.contains("in the clear")),
            "expected the plaintext refusal, got {errors:?}"
        );
    }

    #[test]
    fn plaintext_on_loopback_is_fine() {
        // The dev loop and the e2e harness both live here, and neither is
        // reachable from the network the child is on.
        assert!(
            api_errors("bind = \"127.0.0.1\"\n[service.management_api.tls]\nmode = \"off\"")
                .is_empty()
        );
    }

    #[test]
    fn a_public_bind_that_says_nothing_about_tls_is_fine_and_resolves_to_self_signed() {
        // The whole point of `auto`: saying nothing must not be the insecure
        // answer, and must not be an error either.
        assert!(api_errors("bind = \"0.0.0.0\"").is_empty());
        let cfg: RawConfig = toml::from_str(
            "config_version = 1\n[service.management_api]\nenabled = true\nbind = \"0.0.0.0\"\n",
        )
        .unwrap();
        let policy = crate::policy::Policy::from_raw(cfg);
        assert_eq!(
            policy.service.management_api.unwrap().tls,
            crate::policy::TlsMode::SelfSigned
        );
    }

    #[test]
    fn a_loopback_bind_that_says_nothing_stays_plaintext() {
        let cfg: RawConfig =
            toml::from_str("config_version = 1\n[service.management_api]\nenabled = true\n")
                .unwrap();
        let policy = crate::policy::Policy::from_raw(cfg);
        assert_eq!(
            policy.service.management_api.unwrap().tls,
            crate::policy::TlsMode::Off
        );
    }

    #[test]
    fn files_mode_needs_both_paths() {
        let errors = api_errors("[service.management_api.tls]\nmode = \"files\"");
        assert!(errors.iter().any(|e| e.contains("tls.cert")), "{errors:?}");
        assert!(errors.iter().any(|e| e.contains("tls.key")), "{errors:?}");
    }

    #[test]
    fn a_cert_path_that_is_not_there_is_an_error_rather_than_a_listener_that_never_comes_up() {
        let errors = api_errors(
            "[service.management_api.tls]\nmode = \"files\"\ncert = \"/nope/cert.pem\"\nkey = \"/nope/key.pem\"",
        );
        assert_eq!(errors.len(), 2, "{errors:?}");
        assert!(errors.iter().all(|e| e.contains("does not exist")));
    }

    #[test]
    fn cert_paths_under_a_mode_that_ignores_them_are_called_out() {
        let errors = api_errors(
            "[service.management_api.tls]\nmode = \"self_signed\"\ncert = \"/tmp/c.pem\"",
        );
        assert!(errors.iter().any(|e| e.contains("set mode")), "{errors:?}");
    }

    #[test]
    fn an_unknown_tls_mode_names_the_known_ones() {
        let errors = api_errors("[service.management_api.tls]\nmode = \"letsencrypt\"");
        assert!(
            errors.iter().any(|e| e.contains("self_signed")),
            "{errors:?}"
        );
    }

    #[test]
    fn a_lockout_threshold_of_zero_is_refused() {
        let errors = api_errors("[service.management_api.auth]\nlockout_after = 0");
        assert!(
            errors.iter().any(|e| e.contains("lockout_after")),
            "{errors:?}"
        );
    }

    #[test]
    fn an_idle_timeout_past_the_absolute_one_is_refused() {
        let errors = api_errors(
            "[service.management_api.auth]\nsession_idle_days = 120\nsession_max_days = 90",
        );
        assert!(
            errors.iter().any(|e| e.contains("can never fire")),
            "{errors:?}"
        );
    }

    #[test]
    fn shortening_only_the_absolute_lifetime_is_refused_against_the_default() {
        // The ordinary way to get an idle timeout that can never fire: set one
        // knob, leave the other alone. Only checked when both were written
        // down until the defaults came down to 2 and 14 days, which put the
        // two close enough together for this to be reachable by accident.
        let errors = api_errors("[service.management_api.auth]\nsession_max_days = 1");
        assert!(
            errors.iter().any(|e| e.contains("can never fire")),
            "{errors:?}"
        );
        assert!(
            errors.iter().any(|e| e.contains("(the default)")),
            "the message has to say which value it did not read from the file: {errors:?}"
        );
    }

    #[test]
    fn the_defaults_do_not_refuse_themselves() {
        assert!(api_errors("[service.management_api.auth]\nlockout_after = 8").is_empty());
    }

    #[test]
    fn a_disabled_management_api_is_not_validated() {
        // Nothing is listening, so a plaintext public bind is not a device
        // serving admin in the clear — it is a stanza waiting to be switched on.
        let toml = "config_version = 1\n[service.management_api]\nenabled = false\n\
                    bind = \"0.0.0.0\"\n[service.management_api.tls]\nmode = \"off\"\n";
        let cfg: RawConfig = toml::from_str(toml).expect("parses");
        assert!(validate_config(&cfg).is_empty());
    }
}
