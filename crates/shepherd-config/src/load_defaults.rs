//! The defaults [`Policy::from_raw`](crate::Policy::from_raw) applies to a
//! field that was left unset, gathered somewhere they can be enumerated.
//!
//! # Why these need their own home
//!
//! Most config defaults are `#[serde(default = "…")]`, and those reach the web
//! config editor on their own: `schemars` reads the serde attribute and writes
//! the value into the JSON Schema, which `shepherd-wire-codegen` renders into
//! `field-defaults.generated.ts`.
//!
//! The ones here cannot travel that way. Their fields are `Option<T>` whose
//! `None` means *"fall back"*, and the fallback is applied at policy load —
//! long after deserialization — so `schemars` sees `"default": null` and has
//! nothing to say. The editor was left mirroring the real values by hand:
//! a `const DEFAULT_COOLDOWN_MIN_SESSION = 120` in TypeScript, a
//! `?? 3600`, a `placeholder="2m"`. That is precisely the drift the generated
//! wire types exist to prevent — TypeScript checked against TypeScript cannot
//! see the Rust rule it claims to copy.
//!
//! So the answers are collected here, the way `EntryKindTag` collects the
//! per-kind ones, and generated instead of mirrored.
//!
//! # The guarantee, and its limit
//!
//! [`LoadTimeDefaults::current`] is *advertised*; `Policy::from_raw` is what
//! actually happens. Nothing in the type system ties them together, so
//! `load_defaults_match_what_an_empty_config_parses_to` does: it parses a
//! config that sets none of these fields and asserts the resulting `Policy`
//! agrees with every value below.
//!
//! What that catches is a call site drifting from the constant — the failure
//! this module exists to end, since a bare `unwrap_or(Some(Duration::from_secs(1800)))`
//! in `from_raw` is exactly how the editor came to disagree with the daemon.
//! Verified to bite: replacing `DEFAULT_MAX_RUN` at its call site with a
//! literal fails the test.
//!
//! What it deliberately cannot catch is *changing a constant*, because both
//! sides read the same one and both move together. That is the point rather
//! than a gap: a changed constant flows through `current()` into the generated
//! TypeScript on the next codegen run, and the drift test in
//! `shepherd-wire-codegen` fails until someone does run it.
//!
//! # What is deliberately not here
//!
//! Values that are not defaults at all. `clamp_volume`'s `unwrap_or(0)` /
//! `unwrap_or(100)` are the bounds of a percentage, not a fallback an admin
//! could have configured differently, and the editor's matching `?? 0` / `?? 100`
//! are slider extents rather than a mirror of policy. Adding them here would
//! make the generated file a grab-bag of unrelated literals.

use serde::Serialize;
use shepherd_api::HudOrientation;

use crate::internet::{DEFAULT_INTERNET_CHECK_INTERVAL, DEFAULT_INTERNET_CHECK_TIMEOUT};
use crate::policy::{
    DEFAULT_COOLDOWN_MIN_SESSION, DEFAULT_MANAGEMENT_API_BIND, DEFAULT_MANAGEMENT_API_BIND_RETRY,
    DEFAULT_MANAGEMENT_API_PORT, DEFAULT_MAX_RUN, DEFAULT_SAVE_GRACE, DEFAULT_STEAM_LAUNCH_TIMEOUT,
    DEFAULT_TOKEN_EARN_RATIO, default_warning_thresholds,
};

/// One entry of the default warning schedule.
///
/// Spelled in the units and words `config.toml` uses rather than as a
/// [`WarningThreshold`](shepherd_api::WarningThreshold), because the editor
/// renders the *config* form: an admin turning the section on gets exactly
/// these rows to edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DefaultWarning {
    pub seconds_before: u64,
    /// `info`, `warn` or `critical` — the serde spelling of
    /// [`WarningSeverity`](shepherd_api::WarningSeverity).
    pub severity: &'static str,
}

/// Every default resolved at policy load rather than by serde.
///
/// Field names are the config keys they fill in, and the units are the config's
/// own, so the generated TypeScript can be dropped straight into a control
/// without a conversion the editor would then have to keep in step.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LoadTimeDefaults {
    /// `service.default_max_run_seconds`.
    pub max_run_seconds: u64,
    /// `service.cooldown_min_session_seconds`, and the `[limits]` key of the
    /// same name on an entry or a group.
    pub cooldown_min_session_seconds: u64,
    /// `service.save_grace_seconds`, likewise (issue #155).
    pub save_grace_seconds: u64,
    /// `service.steam.launch_timeout_seconds`.
    pub steam_launch_timeout_seconds: u64,
    /// `service.internet.interval_seconds`.
    pub internet_check_interval_seconds: u64,
    /// `service.internet.timeout_ms`.
    pub internet_check_timeout_ms: u64,
    /// `service.management_api.port`.
    pub management_api_port: u16,
    /// `service.management_api.bind`.
    pub management_api_bind: &'static str,
    /// `service.management_api.bind_retry_seconds`. `0` in the file means
    /// "retry forever", which is why this is the retry *duration* and not an
    /// `Option`.
    pub management_api_bind_retry_seconds: u64,
    /// `tokens.earn_ratio`, on an entry or a group.
    pub token_earn_ratio: f64,
    /// `service.hud.orientation`, and the `hud_orientation` an entry may set
    /// instead (issue #171). An entry with neither inherits this.
    pub hud_orientation: &'static str,
    /// `service.default_warnings`, in the order the daemon emits them.
    pub warnings: Vec<DefaultWarning>,
}

impl LoadTimeDefaults {
    /// What this build of the daemon falls back to.
    ///
    /// Reads the same constants `Policy::from_raw` reads, so the two can only
    /// disagree by someone hardcoding a literal at one of the call sites —
    /// which the module's parse test is there to catch.
    pub fn current() -> Self {
        Self {
            max_run_seconds: DEFAULT_MAX_RUN.as_secs(),
            cooldown_min_session_seconds: DEFAULT_COOLDOWN_MIN_SESSION.as_secs(),
            save_grace_seconds: DEFAULT_SAVE_GRACE.as_secs(),
            steam_launch_timeout_seconds: DEFAULT_STEAM_LAUNCH_TIMEOUT.as_secs(),
            internet_check_interval_seconds: DEFAULT_INTERNET_CHECK_INTERVAL.as_secs(),
            internet_check_timeout_ms: DEFAULT_INTERNET_CHECK_TIMEOUT.as_millis() as u64,
            management_api_port: DEFAULT_MANAGEMENT_API_PORT,
            management_api_bind: DEFAULT_MANAGEMENT_API_BIND,
            management_api_bind_retry_seconds: DEFAULT_MANAGEMENT_API_BIND_RETRY.as_secs(),
            token_earn_ratio: DEFAULT_TOKEN_EARN_RATIO,
            hud_orientation: hud_orientation_wire_name(HudOrientation::default()),
            warnings: default_warning_thresholds()
                .into_iter()
                .map(|w| DefaultWarning {
                    seconds_before: w.seconds_before,
                    severity: severity_wire_name(w.severity),
                })
                .collect(),
        }
    }
}

/// The config spelling of a HUD edge, matching its serde rename.
///
/// Written out for the same reason as the severities below: adding an edge
/// without teaching this about it fails to compile here rather than emitting a
/// string the editor's own types reject.
fn hud_orientation_wire_name(orientation: HudOrientation) -> &'static str {
    match orientation {
        HudOrientation::Top => "top",
        HudOrientation::Bottom => "bottom",
        HudOrientation::Left => "left",
    }
}

/// The config spelling of a severity, matching its serde rename.
///
/// Written out rather than derived so adding a severity without teaching this
/// about it fails to compile here, where it is one line, instead of emitting a
/// string the editor's own types reject.
fn severity_wire_name(severity: shepherd_api::WarningSeverity) -> &'static str {
    use shepherd_api::WarningSeverity as S;
    match severity {
        S::Info => "info",
        S::Warn => "warn",
        S::Critical => "critical",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_config;
    use std::time::Duration;

    /// The whole point of the module: what it advertises has to be what the
    /// parser actually does. A config that sets none of these fields must come
    /// out holding exactly the values above.
    #[test]
    fn load_defaults_match_what_an_empty_config_parses_to() {
        let policy = parse_config(
            r#"
            config_version = 1

            [service.internet]
            check = "https://connectivitycheck.gstatic.com/generate_204"

            [service.management_api]
            enabled = true

            [[entries]]
            id = "plain"
            label = "Plain"
            kind = { type = "process", command = "/usr/bin/game" }

            [[entries]]
            id = "gated"
            label = "Gated"
            kind = { type = "process", command = "/usr/bin/game" }
            [entries.tokens]
            from = ["plain"]
        "#,
        )
        .unwrap();
        let d = LoadTimeDefaults::current();
        let entry = &policy.entries[0];

        assert_eq!(
            entry.limits.max_run,
            Some(Duration::from_secs(d.max_run_seconds))
        );
        assert_eq!(
            entry.limits.cooldown_min_session,
            Duration::from_secs(d.cooldown_min_session_seconds)
        );
        assert_eq!(
            entry.limits.save_grace,
            Duration::from_secs(d.save_grace_seconds)
        );
        assert_eq!(
            policy.service.steam.launch_timeout,
            Duration::from_secs(d.steam_launch_timeout_seconds)
        );

        let internet = policy.service.internet.check.as_ref().unwrap();
        assert_eq!(
            policy.service.internet.interval,
            Duration::from_secs(d.internet_check_interval_seconds)
        );
        assert_eq!(
            policy.service.internet.timeout,
            Duration::from_millis(d.internet_check_timeout_ms)
        );
        let _ = internet;

        let api = policy.service.management_api.as_ref().unwrap();
        assert_eq!(api.port, d.management_api_port);
        assert_eq!(api.bind.to_string(), d.management_api_bind);
        assert_eq!(
            api.bind_retry,
            Some(Duration::from_secs(d.management_api_bind_retry_seconds))
        );

        let tokens = policy.entries[1].tokens.as_ref().unwrap();
        assert_eq!(tokens.earn_ratio, d.token_earn_ratio);

        assert_eq!(
            hud_orientation_wire_name(policy.hud_orientation),
            d.hud_orientation
        );

        let advertised: Vec<_> = d
            .warnings
            .iter()
            .map(|w| (w.seconds_before, w.severity))
            .collect();
        let actual: Vec<_> = entry
            .warnings
            .iter()
            .map(|w| (w.seconds_before, severity_wire_name(w.severity)))
            .collect();
        assert_eq!(advertised, actual);
    }
}
