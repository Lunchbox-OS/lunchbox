//! YouTube playlist URL detection and `Library` construction from pre-fetched
//! video metadata.
//!
//! Network I/O is deliberately absent from this module. The platform binary
//! (e.g. `shepherd-media` on Linux) fetches the playlist via `yt-dlp` and
//! passes the extracted entries to [`build_library_from_entries`], which
//! performs only pure in-memory construction. [android-portability]

use std::path::PathBuf;

use serde::Deserialize;
use url::Url;

use crate::library::{ClassifiedUri, Item, ItemKind, Library, Platform, PosterRef, Source};
use crate::schema::SCHEMA_VERSION;
use crate::uri;

/// Returns `true` if `s` is a YouTube URL that carries a playlist context,
/// i.e. it has a `list` query parameter.
///
/// This is the signal used by the platform binary to decide whether to treat
/// the `--library` value as a library source URL rather than a file path.
/// Individual item URIs inside a library that happen to contain `list=…` are
/// still classified as `ClassifiedUri::YouTube` by [`crate::uri::classify`];
/// the distinction only matters at the top-level dispatch.
pub fn is_youtube_playlist_url(s: &str) -> bool {
    let Ok(url) = Url::parse(s.trim()) else {
        return false;
    };
    let host = url.host_str().unwrap_or("").to_ascii_lowercase();
    if !matches!(
        host.as_str(),
        "youtube.com" | "www.youtube.com" | "m.youtube.com" | "youtu.be"
    ) {
        return false;
    }
    url.query_pairs().any(|(k, _)| k == "list")
}

/// Metadata for a single video entry extracted from a YouTube playlist fetch.
///
/// This struct is produced by the platform binary (e.g. by parsing
/// `yt-dlp --dump-json` output) and passed to [`build_library_from_entries`].
#[derive(Debug, Clone)]
pub struct YoutubePlaylistEntry {
    /// The stable YouTube video ID (e.g. `YE7VzlLtp-4`).
    pub video_id: String,
    /// Display title of the video.
    pub title: String,
    /// Duration in seconds, if known.
    pub duration_seconds: Option<u64>,
    /// Thumbnail URL for use as the item poster, if available.
    pub thumbnail_url: Option<Url>,
}

/// A parsed YouTube playlist: optional title/id and the videos in order.
///
/// Produced by [`parse_flat_playlist`] and consumed by
/// [`build_library_from_entries`]. Both platform binaries run yt-dlp
/// themselves (Linux via a subprocess, Android via a JNI binding) and hand the
/// resulting stdout to the shared parser here.
pub struct PlaylistInfo {
    /// Playlist display title from yt-dlp, if present.
    pub title: Option<String>,
    /// The `list=…` parameter value (YouTube's playlist ID), if present.
    pub playlist_id: Option<String>,
    /// Videos in playlist order.
    pub entries: Vec<YoutubePlaylistEntry>,
}

/// One video object from `yt-dlp --dump-json --flat-playlist`. Only the fields
/// we use are declared; serde ignores the rest. `playlist_title`/`playlist_id`
/// repeat on every entry, so only the first non-`None` value is kept.
#[derive(Debug, Deserialize)]
struct YtDlpEntry {
    id: String,
    title: String,
    #[serde(default)]
    duration: Option<f64>,
    #[serde(default)]
    thumbnail: Option<String>,
    #[serde(default)]
    playlist_title: Option<String>,
    #[serde(default)]
    playlist_id: Option<String>,
}

/// Parse `yt-dlp --dump-json --flat-playlist` output (one JSON object per line)
/// into a [`PlaylistInfo`]. Pure: no network or subprocess. `url` is only used
/// in the "empty playlist" error message. Returns an error string suitable for
/// printing directly to the user.
pub fn parse_flat_playlist(stdout: &str, url: &str) -> Result<PlaylistInfo, String> {
    let mut entries: Vec<YoutubePlaylistEntry> = Vec::new();
    let mut playlist_title: Option<String> = None;
    let mut playlist_id: Option<String> = None;

    for (line_no, line) in stdout.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let entry: YtDlpEntry = serde_json::from_str(line)
            .map_err(|e| format!("failed to parse yt-dlp output (line {line_no}): {e}"))?;

        if playlist_title.is_none() {
            playlist_title = entry.playlist_title;
        }
        if playlist_id.is_none() {
            playlist_id = entry.playlist_id;
        }
        let thumbnail_url = entry.thumbnail.as_deref().and_then(|t| Url::parse(t).ok());
        entries.push(YoutubePlaylistEntry {
            video_id: entry.id,
            title: entry.title,
            duration_seconds: entry.duration.map(|d| d as u64),
            thumbnail_url,
        });
    }

    if entries.is_empty() {
        return Err(format!(
            "no videos found in playlist — check the URL and that the playlist is public: {url}"
        ));
    }

    Ok(PlaylistInfo {
        title: playlist_title,
        playlist_id,
        entries,
    })
}

/// Construct a [`Library`] from pre-fetched YouTube playlist data.
///
/// `playlist_url` is recorded as `source_path` for diagnostics; no relative-
/// path resolution is performed (all posters are remote URLs). `playlist_title`
/// becomes the library title; `playlist_id` (the `list=…` value) drives the
/// `library_id`. If either is absent a reasonable default is derived from the
/// URL.
pub fn build_library_from_entries(
    playlist_url: &str,
    playlist_title: Option<String>,
    playlist_id: Option<&str>,
    entries: &[YoutubePlaylistEntry],
) -> Library {
    let library_id = derive_library_id(playlist_id, playlist_url);
    let title = playlist_title.unwrap_or_else(|| "YouTube Playlist".to_string());

    let items: Vec<Item> = entries
        .iter()
        .enumerate()
        .map(|(i, entry)| build_item(entry, i + 1))
        .collect();

    Library {
        schema_version: SCHEMA_VERSION,
        library_id,
        title,
        items,
        // Store the URL string as the source path for diagnostic messages.
        // Relative-path resolution is never needed for a YouTube library.
        source_path: PathBuf::from(playlist_url),
    }
}

fn build_item(entry: &YoutubePlaylistEntry, index: usize) -> Item {
    let id = sanitize_video_id(&entry.video_id, index);
    let video_url = format!("https://www.youtube.com/watch?v={}", entry.video_id);
    // Url::parse is robust; the only failure case is a truly empty video_id.
    let url = Url::parse(&video_url)
        .unwrap_or_else(|_| Url::parse("https://www.youtube.com/").expect("fallback URL is valid"));

    // Fall back to the derived `i.ytimg.com/.../hqdefault.jpg` URL when yt-dlp
    // didn't provide one. `--flat-playlist` mode in particular returns an
    // empty `thumbnail` field; the real URLs live in a `thumbnails[]` array
    // the platform binary doesn't currently parse.
    let poster = entry
        .thumbnail_url
        .as_ref()
        .map(|u| PosterRef::Remote(u.clone()))
        .or_else(|| uri::youtube_thumbnail_url(&entry.video_id).map(PosterRef::Remote));

    Item {
        id,
        title: entry.title.clone(),
        kind: ItemKind::Video,
        category: None,
        poster,
        duration_seconds: entry.duration_seconds,
        sources: vec![Source {
            platforms: vec![Platform::Any],
            uri: ClassifiedUri::YouTube(url),
            player_hint: None,
        }],
    }
}

/// Derive a `library_id` (matching `[a-z0-9-]+`, max 64 chars) from a YouTube
/// playlist ID such as `PLrEnWoR732-BHrPp_Bief5G7fh6zCe8hb`. Falls back to
/// sanitising the full URL if no playlist ID is provided.
fn derive_library_id(playlist_id: Option<&str>, playlist_url: &str) -> String {
    let raw = playlist_id.unwrap_or(playlist_url);
    let candidate = sanitize_to_id(raw);
    if candidate.is_empty() {
        "youtube-playlist".to_string()
    } else {
        candidate
    }
}

/// Derive a valid item ID from a YouTube video ID.
///
/// YouTube video IDs are 11-char `[A-Za-z0-9_-]` strings. After lowercasing
/// and replacing `_` with `-` they satisfy `[a-z0-9-]+`. Falls back to
/// `video-{index:04}` if the result is empty (should not happen in practice).
fn sanitize_video_id(video_id: &str, index: usize) -> String {
    let candidate = sanitize_to_id(video_id);
    if candidate.is_empty() {
        format!("video-{index:04}")
    } else {
        candidate
    }
}

/// Lowercase all ASCII letters, replace every non-alphanumeric ASCII character
/// with a `-`, collapse consecutive dashes, and strip leading/trailing dashes.
/// Truncate to 64 chars.
fn sanitize_to_id(input: &str) -> String {
    let mut out = String::with_capacity(input.len().min(64));
    let mut last_dash = true; // treat start as after a dash to suppress leading dashes
    for ch in input.chars() {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            out.push(ch);
            last_dash = false;
        } else if ch.is_ascii_uppercase() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
        // Drop anything else, including leading separator characters.
    }
    // Strip trailing dash.
    let trimmed = out.trim_end_matches('-');
    if trimmed.len() > 64 {
        trimmed[..64].trim_end_matches('-').to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse_flat_playlist ---

    const FLAT_JSON: &str = concat!(
        r#"{"id":"abc123","title":"First","duration":61.0,"thumbnail":"https://i.ytimg.com/vi/abc123/hq.jpg","playlist_title":"My List","playlist_id":"PL999"}"#,
        "\n",
        r#"{"id":"def456","title":"Second","duration":120.0}"#,
        "\n",
    );

    #[test]
    fn parses_flat_playlist() {
        let info =
            parse_flat_playlist(FLAT_JSON, "https://youtube.com/playlist?list=PL999").unwrap();
        assert_eq!(info.title.as_deref(), Some("My List"));
        assert_eq!(info.playlist_id.as_deref(), Some("PL999"));
        assert_eq!(info.entries.len(), 2);
        assert_eq!(info.entries[0].video_id, "abc123");
        assert_eq!(info.entries[0].duration_seconds, Some(61));
        assert!(info.entries[0].thumbnail_url.is_some());
        assert_eq!(info.entries[1].title, "Second");
        // playlist_title/id only appear on the first entry; still captured.
        assert!(info.entries[1].thumbnail_url.is_none());
    }

    #[test]
    fn empty_playlist_is_an_error() {
        assert!(parse_flat_playlist("\n  \n", "u").is_err());
    }

    #[test]
    fn malformed_line_is_an_error() {
        assert!(parse_flat_playlist("not json", "u").is_err());
    }

    // --- is_youtube_playlist_url ---

    #[test]
    fn detects_playlist_url() {
        assert!(is_youtube_playlist_url(
            "https://www.youtube.com/playlist?list=PLtest123"
        ));
    }

    #[test]
    fn detects_watch_url_with_list_param() {
        assert!(is_youtube_playlist_url(
            "https://www.youtube.com/watch?v=abc&list=PLtest123"
        ));
    }

    #[test]
    fn detects_youtu_be_with_list() {
        assert!(is_youtube_playlist_url("https://youtu.be/abc?list=PLtest"));
    }

    #[test]
    fn rejects_plain_video_url() {
        assert!(!is_youtube_playlist_url(
            "https://www.youtube.com/watch?v=YE7VzlLtp-4"
        ));
    }

    #[test]
    fn rejects_non_youtube_url() {
        assert!(!is_youtube_playlist_url("https://example.com/?list=PLtest"));
    }

    #[test]
    fn rejects_file_path() {
        assert!(!is_youtube_playlist_url("/srv/media/library.toml"));
    }

    // --- sanitize_to_id ---

    #[test]
    fn sanitize_lowercases_and_collapses() {
        assert_eq!(sanitize_to_id("PLrEnWoR732-BHrPp"), "plrenwor732-bhrpp");
        assert_eq!(sanitize_to_id("abc__DEF"), "abc-def");
        assert_eq!(sanitize_to_id("--leading"), "leading");
        assert_eq!(sanitize_to_id("trailing--"), "trailing");
    }

    #[test]
    fn sanitize_truncates_at_64_chars() {
        let long = "a".repeat(80);
        assert_eq!(sanitize_to_id(&long).len(), 64);
    }

    // --- sanitize_video_id ---

    #[test]
    fn video_id_lowercases_and_preserves_dash() {
        assert_eq!(sanitize_video_id("YE7VzlLtp-4", 1), "ye7vzlltp-4");
    }

    #[test]
    fn video_id_replaces_underscore() {
        assert_eq!(sanitize_video_id("abc_DEF", 1), "abc-def");
    }

    #[test]
    fn video_id_fallback_on_empty() {
        assert_eq!(sanitize_video_id("", 3), "video-0003");
    }

    // --- build_library_from_entries ---

    #[test]
    fn builds_library_with_correct_metadata() {
        let entries = vec![
            YoutubePlaylistEntry {
                video_id: "YE7VzlLtp-4".to_string(),
                title: "Big Buck Bunny".to_string(),
                duration_seconds: Some(596),
                thumbnail_url: None,
            },
            YoutubePlaylistEntry {
                video_id: "abc_123".to_string(),
                title: "Another Video".to_string(),
                duration_seconds: None,
                thumbnail_url: None,
            },
        ];

        let lib = build_library_from_entries(
            "https://www.youtube.com/playlist?list=PLtest",
            Some("My Playlist".to_string()),
            Some("PLtest"),
            &entries,
        );

        assert_eq!(lib.title, "My Playlist");
        assert_eq!(lib.library_id, "pltest");
        assert_eq!(lib.items.len(), 2);

        assert_eq!(lib.items[0].id, "ye7vzlltp-4");
        assert_eq!(lib.items[0].title, "Big Buck Bunny");
        assert_eq!(lib.items[0].duration_seconds, Some(596));
        assert!(matches!(
            lib.items[0].sources[0].uri,
            ClassifiedUri::YouTube(_)
        ));
        assert!(matches!(
            lib.items[0].sources[0].platforms[0],
            Platform::Any
        ));

        assert_eq!(lib.items[1].id, "abc-123");
    }

    #[test]
    fn builds_library_with_thumbnail_poster() {
        let thumb = Url::parse("https://i.ytimg.com/vi/YE7VzlLtp-4/maxresdefault.jpg").unwrap();
        let entries = vec![YoutubePlaylistEntry {
            video_id: "YE7VzlLtp-4".to_string(),
            title: "Test".to_string(),
            duration_seconds: None,
            thumbnail_url: Some(thumb.clone()),
        }];
        let lib = build_library_from_entries(
            "https://www.youtube.com/playlist?list=PL1",
            None,
            Some("PL1"),
            &entries,
        );
        assert!(matches!(
            &lib.items[0].poster,
            Some(PosterRef::Remote(u)) if u == &thumb
        ));
    }

    #[test]
    fn builds_library_derives_thumbnail_when_yt_dlp_omits_it() {
        // `--flat-playlist` returns an empty `thumbnail` field; we should fall
        // back to deriving the `i.ytimg.com/.../hqdefault.jpg` URL.
        let entries = vec![YoutubePlaylistEntry {
            video_id: "YE7VzlLtp-4".to_string(),
            title: "Test".to_string(),
            duration_seconds: None,
            thumbnail_url: None,
        }];
        let lib = build_library_from_entries(
            "https://www.youtube.com/playlist?list=PL1",
            None,
            Some("PL1"),
            &entries,
        );
        match &lib.items[0].poster {
            Some(PosterRef::Remote(u)) => assert_eq!(
                u.as_str(),
                "https://i.ytimg.com/vi/YE7VzlLtp-4/hqdefault.jpg"
            ),
            other => panic!("expected derived poster, got {other:?}"),
        }
    }

    #[test]
    fn default_title_when_none_provided() {
        let lib = build_library_from_entries(
            "https://www.youtube.com/playlist?list=PL1",
            None,
            Some("PL1"),
            &[],
        );
        assert_eq!(lib.title, "YouTube Playlist");
    }

    #[test]
    fn derive_library_id_falls_back_to_url_when_no_id() {
        let id = derive_library_id(None, "https://www.youtube.com/playlist?list=PL123");
        // Should produce something non-empty; exact value depends on sanitization.
        assert!(!id.is_empty());
        assert!(
            id.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        );
    }
}
