//! Fetch YouTube playlist metadata via `yt-dlp`.
//!
//! `yt-dlp` is an external runtime dependency, not a Rust crate dependency.
//! It is invoked as a subprocess. If it is absent, a clear, actionable error
//! is returned rather than a panic.

use std::process::{Command, Stdio};

use serde::Deserialize;
use shepherd_media_core::YoutubePlaylistEntry;
use url::Url;

/// The result of successfully fetching a YouTube playlist.
pub struct PlaylistInfo {
    /// Playlist display title from yt-dlp, if present.
    pub title: Option<String>,
    /// The `list=…` parameter value (YouTube's playlist ID).
    pub playlist_id: Option<String>,
    /// Videos in playlist order.
    pub entries: Vec<YoutubePlaylistEntry>,
}

// Only the fields shepherd-media actually uses are declared; serde ignores
// the rest. `playlist_title` and `playlist_id` repeat on every entry but we
// only keep the first non-None value we see.
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

/// Fetch a YouTube playlist by URL using `yt-dlp`.
///
/// Returns an error string suitable for printing directly to stderr.
///
/// The caller is responsible for having `yt-dlp` installed; see
/// `docs/shepherd-media.md` for setup instructions.
pub fn fetch_playlist(url: &str) -> Result<PlaylistInfo, String> {
    ensure_ytdlp_available()?;

    let output = Command::new("yt-dlp")
        .args([
            "--dump-json",
            "--flat-playlist",
            "--quiet",
            "--no-warnings",
            url,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("failed to run yt-dlp: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(if stderr.trim().is_empty() {
            format!("yt-dlp exited with status {} for URL: {url}", output.status)
        } else {
            format!("yt-dlp error: {}", stderr.trim())
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_ytdlp_output(&stdout, url)
}

fn ensure_ytdlp_available() -> Result<(), String> {
    Command::new("yt-dlp")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| {
            "yt-dlp not found — it is required to use YouTube playlists as libraries.\n\
             Install it with:  pip install yt-dlp\n\
             Or see: https://github.com/yt-dlp/yt-dlp#installation"
                .to_string()
        })?;
    Ok(())
}

fn parse_ytdlp_output(stdout: &str, url: &str) -> Result<PlaylistInfo, String> {
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
