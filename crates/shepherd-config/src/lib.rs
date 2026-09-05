//! Configuration parsing and validation for shepherdd
//!
//! Supports TOML configuration with:
//! - Versioned schema
//! - Entry definitions with availability policies
//! - Time windows, limits, and warnings
//! - Validation with clear error messages

mod icon;
mod internet;
mod policy;
mod schema;
mod validation;

pub use internet::*;
pub use policy::*;
pub use schema::*;
pub use validation::*;

use std::path::Path;
use thiserror::Error;

/// Configuration errors
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("Failed to read config file: {0}")]
    ReadError(#[from] std::io::Error),

    #[error("Failed to parse TOML: {0}")]
    ParseError(#[from] toml::de::Error),

    #[error("Validation failed: {errors:?}")]
    ValidationFailed { errors: Vec<ValidationError> },

    #[error("Unsupported config version: {0}")]
    UnsupportedVersion(u32),
}

pub type ConfigResult<T> = Result<T, ConfigError>;

/// Load and validate configuration from a TOML file
pub fn load_config(path: impl AsRef<Path>) -> ConfigResult<Policy> {
    let content = std::fs::read_to_string(path)?;
    parse_config(&content)
}

/// Parse and validate configuration from a TOML string
pub fn parse_config(content: &str) -> ConfigResult<Policy> {
    let raw: RawConfig = toml::from_str(content)?;

    // Check version
    if raw.config_version != CURRENT_CONFIG_VERSION {
        return Err(ConfigError::UnsupportedVersion(raw.config_version));
    }

    // Validate
    let errors = validate_config(&raw);
    if !errors.is_empty() {
        return Err(ConfigError::ValidationFailed { errors });
    }

    // Convert to policy
    Ok(Policy::from_raw(raw))
}

/// Current supported config version
pub const CURRENT_CONFIG_VERSION: u32 = 1;

#[cfg(test)]
mod tests {
    /// The video cache's size is configuration, not just an environment
    /// variable: shepherdd has to be able to hand the same number to the
    /// activities it launches, which share the cache directory with it.
    #[test]
    fn media_cache_size_and_grace_are_configurable() {
        let policy = parse_config(
            r#"
config_version = 1
[service.media]
cache_max_bytes = 5000000000
watched_grace_days = 90
"#,
        )
        .unwrap();
        assert_eq!(policy.service.media.cache_max_bytes, 5_000_000_000);
        assert_eq!(policy.service.media.watched_grace_days, 90);
    }

    /// The HUD's "are you sure" exists to protect unsaved work. A book has
    /// none — the page is written on the way out — so a reading activity
    /// closes on one tap, while everything else keeps the prompt.
    #[test]
    fn a_reading_activity_closes_without_confirming() {
        let policy = parse_config(
            r#"
config_version = 1

[[entries]]
id = "book"
label = "A Book"
kind = { type = "ebook", book = "~/Books/a.epub" }

[[entries]]
id = "game"
label = "A Game"
kind = { type = "process", command = "/usr/bin/true" }
"#,
        )
        .unwrap();

        let by_id = |id: &str| {
            policy
                .entries
                .iter()
                .find(|e| e.id.as_str() == id)
                .expect("entry exists")
                .confirm_on_close
        };
        assert!(!by_id("book"), "a book has nothing to lose by closing");
        assert!(by_id("game"), "everything else keeps the prompt");
    }

    /// …and an entry that states its preference is obeyed either way, which is
    /// the whole reason the field became optional rather than kind-derived.
    #[test]
    fn an_explicit_confirm_on_close_overrides_the_kind_default() {
        let policy = parse_config(
            r#"
config_version = 1

[[entries]]
id = "book"
label = "A Book"
confirm_on_close = true
kind = { type = "ebook", book = "~/Books/a.epub" }

[[entries]]
id = "game"
label = "A Game"
confirm_on_close = false
kind = { type = "process", command = "/usr/bin/true" }
"#,
        )
        .unwrap();

        let by_id = |id: &str| {
            policy
                .entries
                .iter()
                .find(|e| e.id.as_str() == id)
                .expect("entry exists")
                .confirm_on_close
        };
        assert!(by_id("book"));
        assert!(!by_id("game"));
    }

    #[test]
    fn media_cache_size_and_grace_have_defaults() {
        let policy = parse_config("config_version = 1\n").unwrap();
        assert_eq!(
            policy.service.media.cache_max_bytes,
            10 * 1024 * 1024 * 1024
        );
        assert_eq!(policy.service.media.watched_grace_days, 30);
    }

    use super::*;
    use std::time::Duration;

    #[test]
    fn parse_minimal_config() {
        let config = r#"
            config_version = 1

            [[entries]]
            id = "test-game"
            label = "Test Game"
            kind = { type = "process", command = "/usr/bin/game" }
        "#;

        let policy = parse_config(config).unwrap();
        assert_eq!(policy.entries.len(), 1);
        assert_eq!(policy.entries[0].id.as_str(), "test-game");
    }

    #[test]
    fn reject_wrong_version() {
        let config = r#"
            config_version = 99

            [[entries]]
            id = "test"
            label = "Test"
            kind = { type = "process", command = "/bin/test" }
        "#;

        let result = parse_config(config);
        assert!(matches!(result, Err(ConfigError::UnsupportedVersion(99))));
    }

    #[test]
    fn cooldown_grace_defaults_to_two_minutes() {
        let config = r#"
            config_version = 1

            [[groups]]
            id = "games"
            label = "Games"

            [[entries]]
            id = "no-limits"
            label = "No limits"
            kind = { type = "process", command = "/usr/bin/game" }

            [[entries]]
            id = "with-limits"
            label = "With limits"
            kind = { type = "process", command = "/usr/bin/game" }
            [entries.limits]
            cooldown_seconds = 300
        "#;

        let policy = parse_config(config).unwrap();
        for entry in &policy.entries {
            assert_eq!(
                entry.limits.cooldown_min_session,
                Duration::from_secs(120),
                "{} should get the default grace period",
                entry.id
            );
        }
        assert_eq!(
            policy.groups[0].limits.cooldown_min_session,
            Duration::from_secs(120),
            "a group with no limits table should get it too"
        );
    }

    #[test]
    fn cooldown_grace_service_default_and_per_subject_overrides() {
        let config = r#"
            config_version = 1

            [service]
            cooldown_min_session_seconds = 60

            [[groups]]
            id = "games"
            label = "Games"
            [groups.limits]
            cooldown_seconds = 600
            cooldown_min_session_seconds = 300

            [[entries]]
            id = "inherits"
            label = "Inherits the service default"
            kind = { type = "process", command = "/usr/bin/game" }
            group = "games"
            [entries.limits]
            cooldown_seconds = 300

            [[entries]]
            id = "no-grace"
            label = "Always cools down"
            kind = { type = "process", command = "/usr/bin/game" }
            [entries.limits]
            cooldown_seconds = 300
            cooldown_min_session_seconds = 0
        "#;

        let policy = parse_config(config).unwrap();
        let grace = |id: &str| {
            policy
                .get_entry(&shepherd_util::EntryId::new(id))
                .unwrap()
                .limits
                .cooldown_min_session
        };
        assert_eq!(grace("inherits"), Duration::from_secs(60));
        assert_eq!(grace("no-grace"), Duration::ZERO);
        assert_eq!(
            policy.groups[0].limits.cooldown_min_session,
            Duration::from_secs(300),
            "a group sets its own grace period independently of its members"
        );
    }
}
