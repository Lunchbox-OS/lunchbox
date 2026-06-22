//! Profile identity (which signed bundle a session loads).
//!
//! In a child session the profile is resolved by shepherd-launcher policy and is **not**
//! user-flippable; there is no profile-switch UI. This crate only models the resolved
//! value and maps it to a bundle subdirectory — the actual session→profile decision lives
//! in shepherd-launcher (a config field today; see the host spec, §4.4 / §9.4).

use std::fmt;

/// The safety profile a session runs under. Selects which signed bundle is loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Profile {
    /// Full adult vocabulary.
    Adult,
    /// Child-safe closed vocabulary (profanity is unemittable by construction).
    Child,
}

impl Profile {
    /// The bundle's `profile_id` string, matching `metadata.toml` and the release
    /// artifact layout (`<bundle-root>/adult`, `<bundle-root>/child`).
    pub fn id(self) -> &'static str {
        match self {
            Profile::Adult => "adult",
            Profile::Child => "child",
        }
    }

    /// Parse a profile id (`"adult"` / `"child"`), case-insensitively.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "adult" => Some(Profile::Adult),
            "child" => Some(Profile::Child),
            _ => None,
        }
    }
}

impl fmt::Display for Profile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_round_trips_and_is_case_insensitive() {
        assert_eq!(Profile::parse("adult"), Some(Profile::Adult));
        assert_eq!(Profile::parse("  Child "), Some(Profile::Child));
        assert_eq!(Profile::parse("CHILD"), Some(Profile::Child));
        assert_eq!(Profile::parse("teen"), None);
        assert_eq!(Profile::Adult.id(), "adult");
        assert_eq!(Profile::Child.to_string(), "child");
    }
}
