//! M3U / M3U8 playlist parsing.
//!
//! `shepherd-media`'s native library format is TOML, but a plain `.m3u` or
//! `.m3u8` playlist is often what a user already has on disk. This module
//! parses such a file into the same `Library` shape so the rest of the
//! pipeline can treat it identically. Only the top-level file passed via
//! `--library` is interpreted this way; an `.m3u8` *URI* inside a TOML
//! library still classifies as an HLS direct-HTTP stream.
//!
//! ## Format support
//!
//! - One entry per line, in the order they appear.
//! - Lines beginning with `#` are directives or comments.
//! - `#EXTM3U` (the extended-format marker) is recognised but optional.
//! - `#EXTINF:<seconds>,<title>` immediately preceding an entry sets the
//!   item's title and (when `<seconds> >= 0`) duration.
//! - All other `#` lines are ignored.
//! - Empty lines are skipped.
//!
//! Item IDs are auto-generated as `track-001`, `track-002`, … so the file
//! can be passed straight to `--item` for direct-play. If you need stable
//! ids, convert the playlist to TOML.

use std::path::{Path, PathBuf};

use crate::library::{ClassifiedUri, Item, ItemKind, Library, LibraryError, Platform, Source};
use crate::schema::SCHEMA_VERSION;
use crate::uri::{self, UriError};

/// Audio extensions used to infer `kind = "audio"` for a playlist entry. mpv
/// itself doesn't care; `kind` is only used by the protocol stream.
const AUDIO_EXTENSIONS: &[&str] = &[".mp3", ".flac", ".opus", ".ogg", ".m4a", ".wav"];

/// Parse an M3U/M3U8 playlist into a `Library`.
///
/// `source_path` is used for error messages, deriving the `library_id` and
/// `title`, and resolving relative entry paths.
pub fn parse_playlist(content: &str, source_path: &Path) -> Result<Library, LibraryError> {
    let parent = source_path.parent().unwrap_or(Path::new(""));
    let library_id = derive_library_id(source_path);
    let title = derive_title(source_path);

    let mut items: Vec<Item> = Vec::new();
    let mut pending_title: Option<String> = None;
    let mut pending_duration: Option<u64> = None;
    let mut next_index: usize = 1;

    for (line_no, raw_line) in content.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix('#') {
            // Directive or comment.
            if let Some(extinf) = rest.strip_prefix("EXTINF:") {
                let (duration, title) = parse_extinf(extinf);
                pending_title = title;
                pending_duration = duration;
            }
            // All other directives (EXTM3U, EXTART, EXTGENRE, plain
            // comments, etc.) are ignored. They do not consume the next
            // entry's metadata.
            continue;
        }

        let entry_uri = match resolve_entry_uri(line, parent) {
            Ok(s) => s,
            Err(reason) => {
                return Err(LibraryError::BadUri {
                    path: source_path.to_path_buf(),
                    item_id: format!("track-{:03} (line {})", next_index, line_no + 1),
                    source: UriError::Invalid {
                        uri: line.to_string(),
                        reason,
                    },
                });
            }
        };

        let classified = match uri::classify(&entry_uri) {
            Ok(c) => c,
            Err(UriError::Drm(rej)) => {
                return Err(LibraryError::DrmRejected {
                    path: source_path.to_path_buf(),
                    item_id: format!("track-{:03} (line {})", next_index, line_no + 1),
                    rejection: rej,
                });
            }
            Err(other) => {
                return Err(LibraryError::BadUri {
                    path: source_path.to_path_buf(),
                    item_id: format!("track-{:03} (line {})", next_index, line_no + 1),
                    source: other,
                });
            }
        };

        let id = format!("track-{next_index:03}");
        let title = pending_title
            .take()
            .unwrap_or_else(|| derive_entry_title(line, &classified));
        let kind = infer_kind(&classified);
        let duration_seconds = pending_duration.take();

        items.push(Item {
            id,
            title,
            kind,
            category: None,
            poster: None,
            duration_seconds,
            sources: vec![Source {
                platforms: vec![Platform::Any],
                uri: classified,
                player_hint: None,
            }],
        });
        next_index += 1;
    }

    Ok(Library {
        schema_version: SCHEMA_VERSION,
        library_id,
        title,
        items,
        source_path: source_path.to_path_buf(),
    })
}

fn parse_extinf(rest: &str) -> (Option<u64>, Option<String>) {
    // Format: `<seconds>[ <attrs>],<title>`
    // Anything before the first comma is the duration block; the rest is title.
    // We accept a leading `-1` for live streams (no duration).
    let (duration_part, title_part) = match rest.split_once(',') {
        Some((d, t)) => (d, Some(t.trim().to_string())),
        None => (rest, None),
    };
    // The duration block may also contain key="value" attributes (used by
    // some extended dialects). The duration itself is the first whitespace-
    // delimited token.
    let duration_token = duration_part.split_whitespace().next().unwrap_or("");
    let duration = duration_token
        .parse::<i64>()
        .ok()
        .and_then(|n| if n >= 0 { Some(n as u64) } else { None });
    let title = title_part.filter(|t| !t.is_empty());
    (duration, title)
}

/// Convert a single playlist entry line into a fully-qualified URI string
/// that `uri::classify` can handle.
///
/// Accepts:
/// - Already-qualified URIs (`file://`, `http://`, `https://`, etc.) as-is.
/// - Absolute filesystem paths (`/...`) — wrapped as `file://` URIs.
/// - Relative filesystem paths — resolved against the playlist's directory
///   and wrapped as `file://` URIs.
fn resolve_entry_uri(line: &str, playlist_dir: &Path) -> Result<String, String> {
    if line.contains("://") {
        return Ok(line.to_string());
    }

    let path_buf = if line.starts_with('/') {
        PathBuf::from(line)
    } else {
        playlist_dir.join(line)
    };

    if !path_buf.is_absolute() {
        return Err(
            "relative path could not be resolved (playlist directory has no absolute path)"
                .to_string(),
        );
    }

    let path_str = path_buf
        .to_str()
        .ok_or_else(|| "playlist entry path is not valid UTF-8".to_string())?;
    // url::Url's `from_file_path` is the right tool, but it lives in `url`
    // and we already depend on it transitively via classify(). Use simple
    // string composition: we know the path is absolute UTF-8.
    Ok(format!("file://{path_str}"))
}

fn derive_library_id(source_path: &Path) -> String {
    let stem = source_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("playlist");
    let mut id = String::with_capacity(stem.len());
    for ch in stem.chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            id.push(ch);
        } else if ch.is_ascii_uppercase() {
            id.push(ch.to_ascii_lowercase());
        } else if ch == '-' || ch == '_' || ch == ' ' || ch == '.' {
            // Collapse consecutive separators in a moment.
            id.push('-');
        }
        // Drop anything else.
    }
    // Collapse runs of `-` and trim leading/trailing.
    let mut collapsed = String::with_capacity(id.len());
    let mut last_dash = false;
    for ch in id.chars() {
        if ch == '-' {
            if !last_dash {
                collapsed.push('-');
            }
            last_dash = true;
        } else {
            collapsed.push(ch);
            last_dash = false;
        }
    }
    let trimmed = collapsed.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "playlist".to_string()
    } else if trimmed.len() > 64 {
        trimmed[..64].trim_end_matches('-').to_string()
    } else {
        trimmed
    }
}

fn derive_title(source_path: &Path) -> String {
    source_path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "Playlist".to_string())
}

fn derive_entry_title(line: &str, classified: &ClassifiedUri) -> String {
    match classified {
        ClassifiedUri::Local(path) => path
            .file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| line.to_string()),
        ClassifiedUri::DirectHttp(url)
        | ClassifiedUri::YouTube(url)
        | ClassifiedUri::Unknown(url) => {
            let last = url
                .path_segments()
                .and_then(|mut s| s.next_back())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| url.host_str().unwrap_or("entry"));
            // Strip extension for cleanliness.
            last.rsplit_once('.')
                .map(|(stem, _)| stem)
                .unwrap_or(last)
                .to_string()
        }
    }
}

fn infer_kind(classified: &ClassifiedUri) -> ItemKind {
    let candidate = match classified {
        ClassifiedUri::Local(path) => path.to_string_lossy().to_ascii_lowercase(),
        ClassifiedUri::DirectHttp(url)
        | ClassifiedUri::YouTube(url)
        | ClassifiedUri::Unknown(url) => url.path().to_ascii_lowercase(),
    };
    if AUDIO_EXTENSIONS.iter().any(|ext| candidate.ends_with(ext)) {
        ItemKind::Audio
    } else {
        ItemKind::Video
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_playlist() {
        let lib = parse_playlist(
            "file:///srv/a.mp4\nfile:///srv/b.mp4\n",
            Path::new("/tmp/list.m3u"),
        )
        .unwrap();
        assert_eq!(lib.library_id, "list");
        assert_eq!(lib.items.len(), 2);
        assert_eq!(lib.items[0].id, "track-001");
        assert_eq!(lib.items[1].id, "track-002");
    }

    #[test]
    fn parses_extinf_title_and_duration() {
        let content = "\
#EXTM3U
#EXTINF:120,My Song
file:///srv/song.mp3
#EXTINF:-1,Live Stream
https://example.com/stream.m3u8
";
        let lib = parse_playlist(content, Path::new("/tmp/list.m3u8")).unwrap();
        assert_eq!(lib.items.len(), 2);
        assert_eq!(lib.items[0].title, "My Song");
        assert_eq!(lib.items[0].duration_seconds, Some(120));
        assert_eq!(lib.items[0].kind, ItemKind::Audio);
        assert_eq!(lib.items[1].title, "Live Stream");
        assert_eq!(lib.items[1].duration_seconds, None);
    }

    #[test]
    fn resolves_relative_paths_against_playlist_dir() {
        let lib = parse_playlist("videos/a.mp4\n", Path::new("/srv/media/list.m3u")).unwrap();
        match &lib.items[0].sources[0].uri {
            ClassifiedUri::Local(p) => {
                assert_eq!(p, &PathBuf::from("/srv/media/videos/a.mp4"));
            }
            other => panic!("expected local, got {other:?}"),
        }
    }

    #[test]
    fn rejects_drm_entries() {
        let err = parse_playlist(
            "https://www.netflix.com/watch/123\n",
            Path::new("/tmp/list.m3u"),
        )
        .unwrap_err();
        assert!(matches!(err, LibraryError::DrmRejected { .. }));
    }

    #[test]
    fn ignores_comments_and_blank_lines() {
        let lib = parse_playlist(
            "\n#EXTM3U\n# a comment\n\nfile:///srv/a.mp4\n",
            Path::new("/tmp/list.m3u"),
        )
        .unwrap();
        assert_eq!(lib.items.len(), 1);
    }

    #[test]
    fn extinf_only_applies_to_next_entry() {
        let lib = parse_playlist(
            "#EXTINF:42,First\nfile:///srv/a.mp4\nfile:///srv/b.mp4\n",
            Path::new("/tmp/list.m3u"),
        )
        .unwrap();
        assert_eq!(lib.items[0].title, "First");
        assert_eq!(lib.items[0].duration_seconds, Some(42));
        // Second entry has no EXTINF — title falls back to filename, no duration.
        assert_eq!(lib.items[1].title, "b");
        assert_eq!(lib.items[1].duration_seconds, None);
    }

    #[test]
    fn library_id_collapses_separators() {
        assert_eq!(
            derive_library_id(Path::new("My Cool List.m3u")),
            "my-cool-list"
        );
        assert_eq!(derive_library_id(Path::new("a__b--c.m3u8")), "a-b-c");
    }
}
