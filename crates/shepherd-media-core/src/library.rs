//! Library file types and the `load_library` entry point.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use thiserror::Error;
use url::Url;

use crate::schema::SCHEMA_VERSION;
use crate::uri::{self, DrmRejection, UriError};

/// A parsed and validated library file.
#[derive(Debug, Clone)]
pub struct Library {
    pub schema_version: u32,
    pub library_id: String,
    pub title: String,
    pub items: Vec<Item>,
    pub source_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Item {
    pub id: String,
    pub title: String,
    pub kind: ItemKind,
    pub category: Option<String>,
    pub poster: Option<PosterRef>,
    pub duration_seconds: Option<u64>,
    pub sources: Vec<Source>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemKind {
    Video,
    Audio,
}

#[derive(Debug, Clone)]
pub enum PosterRef {
    Local(PathBuf),
    Remote(Url),
}

#[derive(Debug, Clone)]
pub struct Source {
    pub platforms: Vec<Platform>,
    pub uri: ClassifiedUri,
    pub player_hint: Option<PlayerHint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Linux,
    Android,
    Any,
}

#[derive(Debug, Clone)]
pub enum ClassifiedUri {
    Local(PathBuf),
    DirectHttp(Url),
    YouTube(Url),
    Unknown(Url),
    // RejectedDrm never appears in a valid Library; it's a parse-time error.
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerHint {
    Mpv,
}

#[derive(Debug, Error)]
pub enum LibraryError {
    #[error("failed to read `{path}`: {source}")]
    Read {
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

    #[error("`{path}`: unsupported schema_version: {actual} (this build supports: {supported})")]
    UnsupportedSchema {
        path: PathBuf,
        actual: u32,
        supported: u32,
    },

    #[error("`{path}`: invalid library_id `{value}` ({reason})")]
    InvalidLibraryId {
        path: PathBuf,
        value: String,
        reason: &'static str,
    },

    #[error("`{path}`: invalid title `{value}` ({reason})")]
    InvalidTitle {
        path: PathBuf,
        value: String,
        reason: &'static str,
    },

    #[error("`{path}`: item `{item_id}` has invalid id ({reason})")]
    InvalidItemId {
        path: PathBuf,
        item_id: String,
        reason: &'static str,
    },

    #[error("`{path}`: item `{item_id}` has invalid title ({reason})")]
    InvalidItemTitle {
        path: PathBuf,
        item_id: String,
        reason: &'static str,
    },

    #[error("`{path}`: item `{item_id}` has invalid category ({reason})")]
    InvalidCategory {
        path: PathBuf,
        item_id: String,
        reason: &'static str,
    },

    #[error("`{path}`: item `{item_id}` has empty `sources`")]
    EmptySources { path: PathBuf, item_id: String },

    #[error("`{path}`: item `{item_id}` has a source with empty `platforms`")]
    EmptyPlatforms { path: PathBuf, item_id: String },

    #[error("`{path}`: duplicate item id `{item_id}`")]
    DuplicateItemId { path: PathBuf, item_id: String },

    #[error("`{path}`: item `{item_id}` source URI: {source}")]
    BadUri {
        path: PathBuf,
        item_id: String,
        #[source]
        source: UriError,
    },

    #[error("`{path}`: item `{item_id}` source URI rejected: {rejection}")]
    DrmRejected {
        path: PathBuf,
        item_id: String,
        rejection: DrmRejection,
    },

    #[error("`{path}`: item `{item_id}` poster `{value}`: {reason}")]
    BadPoster {
        path: PathBuf,
        item_id: String,
        value: String,
        reason: String,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawLibrary {
    schema_version: u32,
    library_id: String,
    title: String,
    #[serde(default)]
    items: Vec<RawItem>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawItem {
    id: String,
    title: String,
    kind: RawItemKind,
    category: Option<String>,
    poster: Option<String>,
    duration_seconds: Option<u64>,
    #[serde(default)]
    sources: Vec<RawSource>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum RawItemKind {
    Video,
    Audio,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSource {
    platforms: Vec<String>,
    uri: String,
    player_hint: Option<String>,
}

/// Load a library file from disk.
///
/// The file extension picks the parser:
/// - `.m3u` / `.m3u8` → M3U playlist (auto-derived `library_id`/`title`).
/// - everything else → TOML library (the native format).
///
/// An `.m3u8` URI *inside* a TOML library is an HLS stream, not a nested
/// playlist; that distinction lives in `uri::classify`. Only the top-level
/// file passed here is treated as a playlist.
pub fn load_library(path: &Path) -> Result<Library, LibraryError> {
    let content = std::fs::read_to_string(path).map_err(|e| LibraryError::Read {
        path: path.to_path_buf(),
        source: e,
    })?;
    if is_playlist_extension(path) {
        crate::playlist::parse_playlist(&content, path)
    } else {
        parse_library(&content, path)
    }
}

fn is_playlist_extension(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase()),
        Some(ref e) if e == "m3u" || e == "m3u8"
    )
}

/// Parse a TOML library file from an in-memory string. Public for tests;
/// callers should generally prefer `load_library`.
pub fn parse_library(content: &str, source_path: &Path) -> Result<Library, LibraryError> {
    let raw: RawLibrary = toml::from_str(content).map_err(|e| LibraryError::Parse {
        path: source_path.to_path_buf(),
        source: e,
    })?;

    if raw.schema_version != SCHEMA_VERSION {
        return Err(LibraryError::UnsupportedSchema {
            path: source_path.to_path_buf(),
            actual: raw.schema_version,
            supported: SCHEMA_VERSION,
        });
    }

    validate_library_id(&raw.library_id, source_path)?;
    validate_title(&raw.title, source_path)?;

    let parent = source_path.parent().unwrap_or(Path::new(""));
    let mut items = Vec::with_capacity(raw.items.len());
    let mut seen_ids: HashSet<String> = HashSet::with_capacity(raw.items.len());
    for raw_item in raw.items {
        let item = build_item(raw_item, parent, source_path)?;
        if !seen_ids.insert(item.id.clone()) {
            return Err(LibraryError::DuplicateItemId {
                path: source_path.to_path_buf(),
                item_id: item.id,
            });
        }
        items.push(item);
    }

    Ok(Library {
        schema_version: raw.schema_version,
        library_id: raw.library_id,
        title: raw.title,
        items,
        source_path: source_path.to_path_buf(),
    })
}

fn build_item(raw: RawItem, parent: &Path, source_path: &Path) -> Result<Item, LibraryError> {
    validate_item_id(&raw.id, source_path)?;
    validate_item_title(&raw.id, &raw.title, source_path)?;

    if let Some(category) = &raw.category
        && (category.is_empty() || category.len() > 64)
    {
        return Err(LibraryError::InvalidCategory {
            path: source_path.to_path_buf(),
            item_id: raw.id.clone(),
            reason: "must be 1..=64 chars",
        });
    }

    let kind = match raw.kind {
        RawItemKind::Video => ItemKind::Video,
        RawItemKind::Audio => ItemKind::Audio,
    };

    let poster = match raw.poster {
        Some(s) => Some(parse_poster(&raw.id, &s, parent, source_path)?),
        None => None,
    };

    if raw.sources.is_empty() {
        return Err(LibraryError::EmptySources {
            path: source_path.to_path_buf(),
            item_id: raw.id,
        });
    }

    let mut sources = Vec::with_capacity(raw.sources.len());
    for raw_source in raw.sources {
        sources.push(build_source(&raw.id, raw_source, source_path)?);
    }

    Ok(Item {
        id: raw.id,
        title: raw.title,
        kind,
        category: raw.category,
        poster,
        duration_seconds: raw.duration_seconds,
        sources,
    })
}

fn build_source(item_id: &str, raw: RawSource, source_path: &Path) -> Result<Source, LibraryError> {
    if raw.platforms.is_empty() {
        return Err(LibraryError::EmptyPlatforms {
            path: source_path.to_path_buf(),
            item_id: item_id.to_string(),
        });
    }

    let mut platforms = Vec::with_capacity(raw.platforms.len());
    for p in &raw.platforms {
        platforms.push(parse_platform(item_id, p, source_path)?);
    }

    let uri = match uri::classify(&raw.uri) {
        Ok(c) => c,
        Err(UriError::Drm(rej)) => {
            return Err(LibraryError::DrmRejected {
                path: source_path.to_path_buf(),
                item_id: item_id.to_string(),
                rejection: rej,
            });
        }
        Err(other) => {
            return Err(LibraryError::BadUri {
                path: source_path.to_path_buf(),
                item_id: item_id.to_string(),
                source: other,
            });
        }
    };

    let player_hint = match raw.player_hint.as_deref() {
        None => None,
        Some("mpv") => Some(PlayerHint::Mpv),
        Some(other) => {
            return Err(LibraryError::BadUri {
                path: source_path.to_path_buf(),
                item_id: item_id.to_string(),
                source: UriError::Invalid {
                    uri: raw.uri,
                    reason: format!("unknown player_hint `{other}`"),
                },
            });
        }
    };

    Ok(Source {
        platforms,
        uri,
        player_hint,
    })
}

fn parse_platform(
    item_id: &str,
    value: &str,
    source_path: &Path,
) -> Result<Platform, LibraryError> {
    match value {
        "linux" => Ok(Platform::Linux),
        "android" => Ok(Platform::Android),
        "*" => Ok(Platform::Any),
        _ => Err(LibraryError::BadUri {
            path: source_path.to_path_buf(),
            item_id: item_id.to_string(),
            source: UriError::Invalid {
                uri: value.to_string(),
                reason: format!("unknown platform `{value}` (expected linux, android, or *)"),
            },
        }),
    }
}

fn parse_poster(
    item_id: &str,
    value: &str,
    parent: &Path,
    source_path: &Path,
) -> Result<PosterRef, LibraryError> {
    if value.starts_with("http://") || value.starts_with("https://") {
        let url = Url::parse(value).map_err(|e| LibraryError::BadPoster {
            path: source_path.to_path_buf(),
            item_id: item_id.to_string(),
            value: value.to_string(),
            reason: e.to_string(),
        })?;
        return Ok(PosterRef::Remote(url));
    }

    // Treat anything else as a relative or absolute filesystem path.
    let raw_path = PathBuf::from(value);
    let resolved = if raw_path.is_absolute() {
        raw_path
    } else {
        parent.join(raw_path)
    };
    Ok(PosterRef::Local(resolved))
}

fn validate_library_id(value: &str, source_path: &Path) -> Result<(), LibraryError> {
    if value.is_empty() || value.len() > 64 {
        return Err(LibraryError::InvalidLibraryId {
            path: source_path.to_path_buf(),
            value: value.to_string(),
            reason: "must be 1..=64 chars",
        });
    }
    if !value
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(LibraryError::InvalidLibraryId {
            path: source_path.to_path_buf(),
            value: value.to_string(),
            reason: "must match [a-z0-9-]+",
        });
    }
    Ok(())
}

fn validate_title(value: &str, source_path: &Path) -> Result<(), LibraryError> {
    if value.is_empty() || value.chars().count() > 128 {
        return Err(LibraryError::InvalidTitle {
            path: source_path.to_path_buf(),
            value: value.to_string(),
            reason: "must be 1..=128 chars",
        });
    }
    Ok(())
}

fn validate_item_id(value: &str, source_path: &Path) -> Result<(), LibraryError> {
    if value.is_empty() || value.len() > 64 {
        return Err(LibraryError::InvalidItemId {
            path: source_path.to_path_buf(),
            item_id: value.to_string(),
            reason: "must be 1..=64 chars",
        });
    }
    if !value
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(LibraryError::InvalidItemId {
            path: source_path.to_path_buf(),
            item_id: value.to_string(),
            reason: "must match [a-z0-9-]+",
        });
    }
    Ok(())
}

fn validate_item_title(item_id: &str, value: &str, source_path: &Path) -> Result<(), LibraryError> {
    if value.is_empty() || value.chars().count() > 256 {
        return Err(LibraryError::InvalidItemTitle {
            path: source_path.to_path_buf(),
            item_id: item_id.to_string(),
            reason: "must be 1..=256 chars",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<Library, LibraryError> {
        parse_library(s, Path::new("/tmp/test.toml"))
    }

    #[test]
    fn minimal_valid_library() {
        let lib = parse(
            r#"
                schema_version = 1
                library_id = "demo"
                title = "Demo"

                [[items]]
                id = "a"
                title = "A"
                kind = "video"

                [[items.sources]]
                platforms = ["linux"]
                uri = "file:///srv/a.mp4"
            "#,
        )
        .unwrap();
        assert_eq!(lib.library_id, "demo");
        assert_eq!(lib.items.len(), 1);
    }

    #[test]
    fn rejects_unknown_top_level_key() {
        let err = parse(
            r#"
                schema_version = 1
                library_id = "demo"
                title = "Demo"
                bogus = true
            "#,
        )
        .unwrap_err();
        assert!(matches!(err, LibraryError::Parse { .. }));
    }

    #[test]
    fn rejects_duplicate_item_ids() {
        let err = parse(
            r#"
                schema_version = 1
                library_id = "demo"
                title = "Demo"

                [[items]]
                id = "dup"
                title = "First"
                kind = "video"
                [[items.sources]]
                platforms = ["*"]
                uri = "file:///srv/a.mp4"

                [[items]]
                id = "dup"
                title = "Second"
                kind = "video"
                [[items.sources]]
                platforms = ["*"]
                uri = "file:///srv/b.mp4"
            "#,
        )
        .unwrap_err();
        assert!(matches!(err, LibraryError::DuplicateItemId { .. }));
    }
}
