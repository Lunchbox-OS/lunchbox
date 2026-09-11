//! Configuration parsing and validation for shepherdd
//!
//! Supports TOML configuration with:
//! - Versioned schema
//! - Entry definitions with availability policies
//! - Time windows, limits, and warnings
//! - Validation with clear error messages

/// Administrator mode's `.desktop` enumerator (issue #154).
///
/// Unix-only: it reads the XDG application directories and asks the filesystem
/// which files carry an execute bit, neither of which means anything to the
/// `wasm32-unknown-unknown` build of this crate that the config editor is
/// compiled from. Gating the module rather than the one `PermissionsExt` call
/// keeps that build from carrying a `.desktop` parser it has no device to
/// enumerate.
#[cfg(unix)]
pub mod desktop;
mod icon;
mod internet;
mod load_defaults;
mod policy;
mod schema;
mod validation;

#[cfg(unix)]
pub use desktop::DesktopEntry;
pub use internet::*;
pub use load_defaults::*;
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

    /// A book gets the gamepad preset without asking, because a pad is the one
    /// controller a reading device is likely to have that the reader cannot
    /// use on its own (issue #160).
    #[test]
    fn a_reading_activity_gets_the_gamepad_sidecar() {
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
                .input_compat
                .clone()
        };
        assert_eq!(
            by_id("book"),
            vec![InputCompatMode::GamepadProductivity],
            "a D-pad should turn pages the moment a book opens"
        );
        assert!(
            by_id("game").is_empty(),
            "no other kind gains a sidecar it did not ask for"
        );
    }

    /// A listed `input_compat` replaces the kind's answer rather than adding to
    /// it, and an empty list is a list — the way an entry says "no sidecar".
    #[test]
    fn an_explicit_input_compat_overrides_the_kind_default() {
        let policy = parse_config(
            r#"
config_version = 1

[[entries]]
id = "touch-book"
label = "A Book"
input_compat = "touch_to_mouse"
kind = { type = "ebook", book = "~/Books/a.epub" }

[[entries]]
id = "bare-book"
label = "Another Book"
input_compat = []
kind = { type = "ebook", book = "~/Books/b.epub" }
"#,
        )
        .unwrap();

        let by_id = |id: &str| {
            policy
                .entries
                .iter()
                .find(|e| e.id.as_str() == id)
                .expect("entry exists")
                .input_compat
                .clone()
        };
        assert_eq!(by_id("touch-book"), vec![InputCompatMode::TouchToMouse]);
        assert!(by_id("bare-book").is_empty());
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
    use shepherd_api::InputCompatMode;
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

    /// The save-progress grace (issue #155) is the one limit that cascades
    /// service -> group -> entry, so a "bedtime" category can set it once.
    #[test]
    fn save_grace_cascades_from_service_through_the_group_to_the_entry() {
        let config = r#"
            config_version = 1

            [service]
            save_grace_seconds = 30

            [[groups]]
            id = "bedtime"
            label = "Bedtime"
            [groups.limits]
            save_grace_seconds = 300

            [[entries]]
            id = "inherits-group"
            label = "Inherits the category"
            kind = { type = "process", command = "/usr/bin/game" }
            group = "bedtime"

            [[entries]]
            id = "own-value"
            label = "Sets its own"
            kind = { type = "process", command = "/usr/bin/game" }
            group = "bedtime"
            [entries.limits]
            save_grace_seconds = 45

            [[entries]]
            id = "ungrouped"
            label = "No category"
            kind = { type = "process", command = "/usr/bin/game" }

            [[entries]]
            id = "no-grace"
            label = "Cut off on wake"
            kind = { type = "process", command = "/usr/bin/game" }
            [entries.limits]
            save_grace_seconds = 0
        "#;

        let policy = parse_config(config).unwrap();
        let grace = |id: &str| {
            policy
                .get_entry(&shepherd_util::EntryId::new(id))
                .unwrap()
                .limits
                .save_grace
        };
        assert_eq!(grace("inherits-group"), Duration::from_secs(300));
        assert_eq!(grace("own-value"), Duration::from_secs(45));
        assert_eq!(grace("ungrouped"), Duration::from_secs(30));
        assert_eq!(grace("no-grace"), Duration::ZERO);
    }

    #[test]
    fn save_grace_defaults_to_two_minutes() {
        let config = r#"
            config_version = 1

            [[groups]]
            id = "games"
            label = "Games"

            [[entries]]
            id = "plain"
            label = "Plain"
            kind = { type = "process", command = "/usr/bin/game" }
        "#;

        let policy = parse_config(config).unwrap();
        assert_eq!(
            policy.entries[0].limits.save_grace,
            Duration::from_secs(120)
        );
        assert_eq!(
            policy.groups[0].limits.save_grace,
            Duration::from_secs(120),
            "a group with no limits table should get it too"
        );
    }
}
