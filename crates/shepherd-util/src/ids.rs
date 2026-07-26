//! Strongly-typed identifiers for shepherdd

use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

/// Unique identifier for an entry in the policy whitelist
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schema", schemars(transparent))]
pub struct EntryId(String);

impl EntryId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EntryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for EntryId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for EntryId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// Unique identifier for a group of entries sharing a schedule and limits
/// (issue #5)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schema", schemars(transparent))]
pub struct GroupId(String);

impl GroupId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for GroupId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for GroupId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for GroupId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

/// Prefix distinguishing a group from an entry in a [`LimitSubject`] key.
pub const GROUP_SUBJECT_PREFIX: &str = "group:";

/// Something a limit can be attached to: an individual entry, or a group of
/// them (issue #5).
///
/// Cooldowns, token balances, and daily overrides are all keyed by a subject so
/// that a group can carry the same state an entry can.
///
/// The string form of an entry subject is the bare entry ID, and only groups
/// take the `group:` prefix. That keeps every pre-existing entry-keyed row and
/// API call valid without rewriting them — which is why entry IDs are forbidden
/// from starting with `group:` at config-validation time.
// Hand-written Serialize/Deserialize render this as a single string
// (`<entry-id>` / `group:<id>`), so the schema must say "string" rather than
// describe the enum shape.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schema", schemars(with = "String"))]
pub enum LimitSubject {
    Entry(EntryId),
    Group(GroupId),
}

impl LimitSubject {
    pub fn entry(id: impl Into<String>) -> Self {
        Self::Entry(EntryId::new(id))
    }

    pub fn group(id: impl Into<String>) -> Self {
        Self::Group(GroupId::new(id))
    }

    /// The entry ID, if this subject is an entry.
    pub fn as_entry(&self) -> Option<&EntryId> {
        match self {
            Self::Entry(id) => Some(id),
            Self::Group(_) => None,
        }
    }

    /// The group ID, if this subject is a group.
    pub fn as_group(&self) -> Option<&GroupId> {
        match self {
            Self::Group(id) => Some(id),
            Self::Entry(_) => None,
        }
    }
}

impl fmt::Display for LimitSubject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Entry(id) => write!(f, "{id}"),
            Self::Group(id) => write!(f, "{GROUP_SUBJECT_PREFIX}{id}"),
        }
    }
}

impl std::str::FromStr for LimitSubject {
    type Err = std::convert::Infallible;

    /// Never fails: an unprefixed string is an entry ID, which is what every
    /// pre-group caller and stored row looks like.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.strip_prefix(GROUP_SUBJECT_PREFIX) {
            Some(group) => Self::Group(GroupId::new(group)),
            None => Self::Entry(EntryId::new(s)),
        })
    }
}

impl From<EntryId> for LimitSubject {
    fn from(id: EntryId) -> Self {
        Self::Entry(id)
    }
}

impl From<GroupId> for LimitSubject {
    fn from(id: GroupId) -> Self {
        Self::Group(id)
    }
}

impl Serialize for LimitSubject {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for LimitSubject {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(s.parse().expect("LimitSubject parsing is infallible"))
    }
}

/// Unique identifier for a running session
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schema", schemars(transparent))]
pub struct SessionId(Uuid);

impl SessionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Unique identifier for a connected IPC client
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[cfg_attr(feature = "schema", schemars(transparent))]
pub struct ClientId(Uuid);

impl ClientId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }
}

impl Default for ClientId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for ClientId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_id_equality() {
        let id1 = EntryId::new("game-1");
        let id2 = EntryId::new("game-1");
        let id3 = EntryId::new("game-2");

        assert_eq!(id1, id2);
        assert_ne!(id1, id3);
    }

    #[test]
    fn session_id_uniqueness() {
        let s1 = SessionId::new();
        let s2 = SessionId::new();
        assert_ne!(s1, s2);
    }

    #[test]
    fn limit_subject_string_form_is_backward_compatible() {
        // An entry subject is the bare ID, so rows and API calls written before
        // groups existed still parse and still match on lookup.
        assert_eq!(LimitSubject::entry("tuxmath").to_string(), "tuxmath");
        assert_eq!(
            "tuxmath".parse::<LimitSubject>().unwrap(),
            LimitSubject::entry("tuxmath")
        );

        // Only groups take a prefix.
        assert_eq!(LimitSubject::group("games").to_string(), "group:games");
        assert_eq!(
            "group:games".parse::<LimitSubject>().unwrap(),
            LimitSubject::group("games")
        );
    }

    #[test]
    fn limit_subject_round_trips_through_json_as_a_string() {
        for subject in [LimitSubject::entry("tuxmath"), LimitSubject::group("games")] {
            let json = serde_json::to_string(&subject).unwrap();
            assert!(
                json.starts_with('"'),
                "should serialize as a string: {json}"
            );
            let parsed: LimitSubject = serde_json::from_str(&json).unwrap();
            assert_eq!(subject, parsed);
        }
    }

    #[test]
    fn ids_serialize_deserialize() {
        let entry_id = EntryId::new("test-entry");
        let json = serde_json::to_string(&entry_id).unwrap();
        let parsed: EntryId = serde_json::from_str(&json).unwrap();
        assert_eq!(entry_id, parsed);

        let session_id = SessionId::new();
        let json = serde_json::to_string(&session_id).unwrap();
        let parsed: SessionId = serde_json::from_str(&json).unwrap();
        assert_eq!(session_id, parsed);
    }
}
