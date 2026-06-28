//! Persistent application settings: the configured libraries, their caching
//! options, and which one is currently selected.
//!
//! This is the state the Linux binary never needs (because `shepherdd` passes a
//! single `--library` per activity) but the Android build does (one install,
//! many libraries, a settings page to manage them, and a switcher to pick one).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::quality::{CacheMode, PosterPolicy, Quality};

/// Schema version of the on-disk settings file. Bump when the shape changes in
/// a backward-incompatible way.
pub const SETTINGS_SCHEMA_VERSION: u32 = 1;

/// Default per-library video cache cap (5 GiB). Smaller than the Linux binary's
/// 10 GiB default because phones and tablets have tighter storage budgets.
pub const DEFAULT_MAX_CACHE_BYTES: u64 = 5 * 1024 * 1024 * 1024;

/// The full persisted application state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppSettings {
    pub schema_version: u32,
    /// Id of the library shown on launch. Must reference an entry in
    /// [`libraries`](Self::libraries). `None` only when no libraries are
    /// configured yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_library: Option<String>,
    #[serde(default)]
    pub libraries: Vec<LibraryEntry>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            schema_version: SETTINGS_SCHEMA_VERSION,
            active_library: None,
            libraries: Vec::new(),
        }
    }
}

/// One configured library: an id/label, where it comes from, and how it caches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LibraryEntry {
    /// Stable identifier, `[a-z0-9-]+`, 1..=64 chars. Unique within the file.
    pub id: String,
    /// Human-facing name shown in the switcher and settings list.
    pub label: String,
    pub source: LibrarySource,
    #[serde(default)]
    pub caching: CachingSettings,
}

/// Where a library's content comes from. The variants mirror the dispatch the
/// Linux binary already performs on its `--library` argument: a local/SAF TOML
/// file, an `http(s)` TOML file, an `.m3u`/`.m3u8` playlist, or a YouTube
/// playlist URL. Resolving a variant into a `shepherd_media_core::Library`
/// (which may require network or a `ContentResolver`) is the platform binary's
/// job, not this crate's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum LibrarySource {
    /// A TOML library reached through a persisted Android SAF `content://` URI.
    SafToml { uri: String },
    /// A TOML library fetched over HTTP(S).
    HttpToml { url: String },
    /// An `.m3u`/`.m3u8` playlist reached through a SAF or filesystem URI.
    M3u { uri: String },
    /// A YouTube playlist URL (resolved via the bundled yt-dlp at runtime).
    YoutubePlaylist { url: String },
}

impl LibrarySource {
    /// The user-supplied locator (URI or URL) regardless of variant. Used for
    /// validation and display.
    pub fn locator(&self) -> &str {
        match self {
            LibrarySource::SafToml { uri } | LibrarySource::M3u { uri } => uri,
            LibrarySource::HttpToml { url } | LibrarySource::YoutubePlaylist { url } => url,
        }
    }
}

/// Per-library caching and quality knobs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CachingSettings {
    #[serde(default)]
    pub mode: CacheMode,
    /// Maximum bytes this library may occupy in the on-disk video cache.
    #[serde(default = "default_max_bytes")]
    pub max_bytes: u64,
    #[serde(default)]
    pub posters: PosterPolicy,
    #[serde(default)]
    pub quality: Quality,
}

impl Default for CachingSettings {
    fn default() -> Self {
        Self {
            mode: CacheMode::default(),
            max_bytes: DEFAULT_MAX_CACHE_BYTES,
            posters: PosterPolicy::default(),
            quality: Quality::default(),
        }
    }
}

fn default_max_bytes() -> u64 {
    DEFAULT_MAX_CACHE_BYTES
}

/// Errors from constructing or mutating [`AppSettings`].
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SettingsError {
    #[error("unsupported settings schema_version {actual} (this build supports {supported})")]
    UnsupportedSchema { actual: u32, supported: u32 },

    #[error("invalid library id `{id}` ({reason})")]
    InvalidId { id: String, reason: &'static str },

    #[error("library `{id}` has an invalid label ({reason})")]
    InvalidLabel { id: String, reason: &'static str },

    #[error("library `{id}` has an empty source locator")]
    EmptySource { id: String },

    #[error("duplicate library id `{0}`")]
    DuplicateId(String),

    #[error("no library with id `{0}`")]
    UnknownId(String),

    #[error("active_library `{0}` does not match any configured library")]
    DanglingActive(String),
}

/// Errors from reading or writing the settings file.
#[derive(Debug, Error)]
pub enum SettingsIoError {
    #[error("failed to read `{path}`: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to write `{path}`: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse `{path}`: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },

    #[error("failed to serialize settings: {0}")]
    Serialize(#[from] toml::ser::Error),

    #[error(transparent)]
    Invalid(#[from] SettingsError),
}

impl AppSettings {
    /// A fresh, empty settings object at the current schema version.
    pub fn new() -> Self {
        Self::default()
    }

    /// The currently selected library, if any.
    pub fn active(&self) -> Option<&LibraryEntry> {
        let id = self.active_library.as_deref()?;
        self.get(id)
    }

    /// Look up a library by id.
    pub fn get(&self, id: &str) -> Option<&LibraryEntry> {
        self.libraries.iter().find(|e| e.id == id)
    }

    /// Mutable lookup by id (e.g. to edit caching options in place).
    pub fn get_mut(&mut self, id: &str) -> Option<&mut LibraryEntry> {
        self.libraries.iter_mut().find(|e| e.id == id)
    }

    /// Validate a candidate entry in isolation (id/label/source well-formed).
    fn validate_entry(entry: &LibraryEntry) -> Result<(), SettingsError> {
        validate_id(&entry.id)?;
        validate_label(&entry.id, &entry.label)?;
        if entry.source.locator().trim().is_empty() {
            return Err(SettingsError::EmptySource {
                id: entry.id.clone(),
            });
        }
        Ok(())
    }

    /// Check the whole object is internally consistent: schema is supported,
    /// every entry is well-formed, ids are unique, and `active_library` (if set)
    /// points at a real entry.
    pub fn validate(&self) -> Result<(), SettingsError> {
        if self.schema_version != SETTINGS_SCHEMA_VERSION {
            return Err(SettingsError::UnsupportedSchema {
                actual: self.schema_version,
                supported: SETTINGS_SCHEMA_VERSION,
            });
        }
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for entry in &self.libraries {
            Self::validate_entry(entry)?;
            if !seen.insert(entry.id.as_str()) {
                return Err(SettingsError::DuplicateId(entry.id.clone()));
            }
        }
        if let Some(active) = &self.active_library
            && self.get(active).is_none()
        {
            return Err(SettingsError::DanglingActive(active.clone()));
        }
        Ok(())
    }

    /// Add a library. Validates the entry and rejects a duplicate id. When this
    /// is the first library, it becomes the active one.
    pub fn add_library(&mut self, entry: LibraryEntry) -> Result<(), SettingsError> {
        Self::validate_entry(&entry)?;
        if self.get(&entry.id).is_some() {
            return Err(SettingsError::DuplicateId(entry.id));
        }
        if self.active_library.is_none() {
            self.active_library = Some(entry.id.clone());
        }
        self.libraries.push(entry);
        Ok(())
    }

    /// Remove a library by id, returning the removed entry. If the removed
    /// library was active, the selection moves to the first remaining library
    /// (or `None` when the list is now empty).
    pub fn remove_library(&mut self, id: &str) -> Result<LibraryEntry, SettingsError> {
        let idx = self
            .libraries
            .iter()
            .position(|e| e.id == id)
            .ok_or_else(|| SettingsError::UnknownId(id.to_string()))?;
        let removed = self.libraries.remove(idx);
        if self.active_library.as_deref() == Some(id) {
            self.active_library = self.libraries.first().map(|e| e.id.clone());
        }
        Ok(removed)
    }

    /// Select the active library. Errors if no library has that id.
    pub fn set_active(&mut self, id: &str) -> Result<(), SettingsError> {
        if self.get(id).is_none() {
            return Err(SettingsError::UnknownId(id.to_string()));
        }
        self.active_library = Some(id.to_string());
        Ok(())
    }

    /// Move a library to `new_index` in the list, shifting the others. Used to
    /// reorder the switcher. `new_index` is clamped to the valid range.
    pub fn move_library(&mut self, id: &str, new_index: usize) -> Result<(), SettingsError> {
        let cur = self
            .libraries
            .iter()
            .position(|e| e.id == id)
            .ok_or_else(|| SettingsError::UnknownId(id.to_string()))?;
        let dest = new_index.min(self.libraries.len() - 1);
        if cur == dest {
            return Ok(());
        }
        let entry = self.libraries.remove(cur);
        self.libraries.insert(dest, entry);
        Ok(())
    }

    // --- persistence -------------------------------------------------------

    /// Parse settings from a TOML string and validate them.
    pub fn from_toml_str(s: &str) -> Result<Self, SettingsIoError> {
        // Parse with a synthetic path for nicer errors; real callers use `load`.
        Self::from_toml_str_at(s, Path::new("<settings>"))
    }

    fn from_toml_str_at(s: &str, path: &Path) -> Result<Self, SettingsIoError> {
        let settings: AppSettings = toml::from_str(s).map_err(|e| SettingsIoError::Parse {
            path: path.to_path_buf(),
            source: e,
        })?;
        settings.validate()?;
        Ok(settings)
    }

    /// Serialize settings to a TOML string. Validates first so we never persist
    /// an inconsistent file.
    pub fn to_toml_str(&self) -> Result<String, SettingsIoError> {
        self.validate()?;
        Ok(toml::to_string_pretty(self)?)
    }

    /// Load settings from `path`. A missing file yields fresh, empty settings —
    /// the normal first-launch case.
    pub fn load(path: &Path) -> Result<Self, SettingsIoError> {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::new()),
            Err(e) => {
                return Err(SettingsIoError::Read {
                    path: path.to_path_buf(),
                    source: e,
                });
            }
        };
        Self::from_toml_str_at(&content, path)
    }

    /// Atomically save settings to `path` (write to a sibling temp file, then
    /// rename) so a crash mid-write can't truncate the existing file.
    pub fn save(&self, path: &Path) -> Result<(), SettingsIoError> {
        let body = self.to_toml_str()?;
        let tmp = tmp_sibling(path);
        std::fs::write(&tmp, body).map_err(|e| SettingsIoError::Write {
            path: tmp.clone(),
            source: e,
        })?;
        std::fs::rename(&tmp, path).map_err(|e| SettingsIoError::Write {
            path: path.to_path_buf(),
            source: e,
        })
    }
}

/// `foo.toml` -> `foo.toml.tmp`; a bare name still gets a `.tmp` sibling.
fn tmp_sibling(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

/// Library ids share the library-file rule: `[a-z0-9-]+`, 1..=64 chars.
fn validate_id(id: &str) -> Result<(), SettingsError> {
    if id.is_empty() || id.len() > 64 {
        return Err(SettingsError::InvalidId {
            id: id.to_string(),
            reason: "must be 1..=64 chars",
        });
    }
    if !id
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(SettingsError::InvalidId {
            id: id.to_string(),
            reason: "must match [a-z0-9-]+",
        });
    }
    Ok(())
}

fn validate_label(id: &str, label: &str) -> Result<(), SettingsError> {
    if label.is_empty() || label.chars().count() > 128 {
        return Err(SettingsError::InvalidLabel {
            id: id.to_string(),
            reason: "must be 1..=128 chars",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str) -> LibraryEntry {
        LibraryEntry {
            id: id.to_string(),
            label: format!("Label {id}"),
            source: LibrarySource::HttpToml {
                url: "https://example.com/lib.toml".to_string(),
            },
            caching: CachingSettings::default(),
        }
    }

    #[test]
    fn new_is_empty_and_valid() {
        let s = AppSettings::new();
        assert_eq!(s.schema_version, SETTINGS_SCHEMA_VERSION);
        assert!(s.libraries.is_empty());
        assert!(s.active().is_none());
        s.validate().unwrap();
    }

    #[test]
    fn first_added_library_becomes_active() {
        let mut s = AppSettings::new();
        s.add_library(entry("kids")).unwrap();
        assert_eq!(s.active_library.as_deref(), Some("kids"));
        s.add_library(entry("lofi")).unwrap();
        // Adding a second does not change the active selection.
        assert_eq!(s.active_library.as_deref(), Some("kids"));
        assert_eq!(s.active().unwrap().id, "kids");
    }

    #[test]
    fn duplicate_id_rejected() {
        let mut s = AppSettings::new();
        s.add_library(entry("dup")).unwrap();
        assert_eq!(
            s.add_library(entry("dup")).unwrap_err(),
            SettingsError::DuplicateId("dup".to_string())
        );
    }

    #[test]
    fn invalid_id_rejected() {
        let mut s = AppSettings::new();
        let err = s.add_library(entry("Bad_Id")).unwrap_err();
        assert!(matches!(err, SettingsError::InvalidId { .. }));
    }

    #[test]
    fn empty_source_rejected() {
        let mut s = AppSettings::new();
        let mut e = entry("x");
        e.source = LibrarySource::SafToml {
            uri: "   ".to_string(),
        };
        assert_eq!(
            s.add_library(e).unwrap_err(),
            SettingsError::EmptySource {
                id: "x".to_string()
            }
        );
    }

    #[test]
    fn removing_active_repoints_to_first_remaining() {
        let mut s = AppSettings::new();
        s.add_library(entry("a")).unwrap();
        s.add_library(entry("b")).unwrap();
        s.add_library(entry("c")).unwrap();
        s.set_active("b").unwrap();
        let removed = s.remove_library("b").unwrap();
        assert_eq!(removed.id, "b");
        // "b" was active; selection falls back to the first remaining ("a").
        assert_eq!(s.active_library.as_deref(), Some("a"));
    }

    #[test]
    fn removing_inactive_keeps_selection() {
        let mut s = AppSettings::new();
        s.add_library(entry("a")).unwrap();
        s.add_library(entry("b")).unwrap();
        s.remove_library("b").unwrap();
        assert_eq!(s.active_library.as_deref(), Some("a"));
    }

    #[test]
    fn removing_last_clears_active() {
        let mut s = AppSettings::new();
        s.add_library(entry("only")).unwrap();
        s.remove_library("only").unwrap();
        assert!(s.active_library.is_none());
        s.validate().unwrap();
    }

    #[test]
    fn remove_unknown_errors() {
        let mut s = AppSettings::new();
        assert_eq!(
            s.remove_library("nope").unwrap_err(),
            SettingsError::UnknownId("nope".to_string())
        );
    }

    #[test]
    fn set_active_unknown_errors() {
        let mut s = AppSettings::new();
        s.add_library(entry("a")).unwrap();
        assert_eq!(
            s.set_active("ghost").unwrap_err(),
            SettingsError::UnknownId("ghost".to_string())
        );
    }

    #[test]
    fn move_library_reorders_and_clamps() {
        let mut s = AppSettings::new();
        for id in ["a", "b", "c"] {
            s.add_library(entry(id)).unwrap();
        }
        s.move_library("a", 99).unwrap(); // clamps to last
        let order: Vec<&str> = s.libraries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(order, ["b", "c", "a"]);
        s.move_library("a", 0).unwrap();
        let order: Vec<&str> = s.libraries.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(order, ["a", "b", "c"]);
    }

    #[test]
    fn round_trips_through_toml() {
        let mut s = AppSettings::new();
        let mut kids = entry("kids");
        kids.source = LibrarySource::SafToml {
            uri: "content://com.android.providers/movies.toml".to_string(),
        };
        kids.caching = CachingSettings {
            mode: CacheMode::QueueAfterPlay,
            max_bytes: 1_000_000,
            posters: PosterPolicy::WifiOnly,
            quality: Quality::Q720,
        };
        s.add_library(kids).unwrap();
        s.add_library(LibraryEntry {
            id: "lofi".to_string(),
            label: "Lofi Beats".to_string(),
            source: LibrarySource::YoutubePlaylist {
                url: "https://www.youtube.com/playlist?list=UUtest".to_string(),
            },
            caching: CachingSettings::default(),
        })
        .unwrap();

        let toml = s.to_toml_str().unwrap();
        let back = AppSettings::from_toml_str(&toml).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn all_source_variants_round_trip() {
        let variants = [
            LibrarySource::SafToml {
                uri: "content://x".to_string(),
            },
            LibrarySource::HttpToml {
                url: "https://x/lib.toml".to_string(),
            },
            LibrarySource::M3u {
                uri: "content://y.m3u".to_string(),
            },
            LibrarySource::YoutubePlaylist {
                url: "https://youtube.com/playlist?list=PL".to_string(),
            },
        ];
        for (i, v) in variants.into_iter().enumerate() {
            let mut s = AppSettings::new();
            let mut e = entry(&format!("lib-{i}"));
            e.source = v.clone();
            s.add_library(e).unwrap();
            let back = AppSettings::from_toml_str(&s.to_toml_str().unwrap()).unwrap();
            assert_eq!(back.libraries[0].source, v);
        }
    }

    #[test]
    fn source_kind_spelling_is_kebab_case() {
        let mut s = AppSettings::new();
        let mut e = entry("yt");
        e.source = LibrarySource::YoutubePlaylist {
            url: "https://youtube.com/playlist?list=PL".to_string(),
        };
        s.add_library(e).unwrap();
        let toml = s.to_toml_str().unwrap();
        assert!(
            toml.contains("kind = \"youtube-playlist\""),
            "expected kebab-case kind, got:\n{toml}"
        );
    }

    #[test]
    fn unknown_field_rejected() {
        let err = AppSettings::from_toml_str(
            r#"
                schema_version = 1
                bogus = true
            "#,
        )
        .unwrap_err();
        assert!(matches!(err, SettingsIoError::Parse { .. }));
    }

    #[test]
    fn dangling_active_rejected_on_parse() {
        let err = AppSettings::from_toml_str(
            r#"
                schema_version = 1
                active_library = "ghost"
            "#,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            SettingsIoError::Invalid(SettingsError::DanglingActive(_))
        ));
    }

    #[test]
    fn unsupported_schema_rejected() {
        let err = AppSettings::from_toml_str("schema_version = 999").unwrap_err();
        assert!(matches!(
            err,
            SettingsIoError::Invalid(SettingsError::UnsupportedSchema { actual: 999, .. })
        ));
    }

    #[test]
    fn caching_defaults_fill_in_missing_fields() {
        let s = AppSettings::from_toml_str(
            r#"
                schema_version = 1
                active_library = "a"

                [[libraries]]
                id = "a"
                label = "A"
                source = { kind = "http-toml", url = "https://x/lib.toml" }
            "#,
        )
        .unwrap();
        let c = &s.libraries[0].caching;
        assert_eq!(c.mode, CacheMode::Off);
        assert_eq!(c.max_bytes, DEFAULT_MAX_CACHE_BYTES);
        assert_eq!(c.posters, PosterPolicy::Always);
        assert_eq!(c.quality, Quality::Q1080);
    }

    #[test]
    fn load_missing_file_yields_empty() {
        let dir = std::env::temp_dir().join("shepherd-media-app-test-missing");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("does-not-exist.toml");
        let s = AppSettings::load(&path).unwrap();
        assert_eq!(s, AppSettings::new());
    }

    #[test]
    fn save_then_load_is_identity() {
        let dir = std::env::temp_dir().join("shepherd-media-app-test-save");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.toml");
        let mut s = AppSettings::new();
        s.add_library(entry("a")).unwrap();
        s.save(&path).unwrap();
        let back = AppSettings::load(&path).unwrap();
        assert_eq!(s, back);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
