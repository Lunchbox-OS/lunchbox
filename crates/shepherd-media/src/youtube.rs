//! Fetch YouTube playlist metadata via `yt-dlp`, with an on-disk cache.
//!
//! `yt-dlp` is an external runtime dependency, not a Rust crate dependency.
//! It is invoked as a subprocess. If it is absent, a clear, actionable error
//! is returned rather than a panic.
//!
//! Fetched playlist metadata is cached in
//! `$XDG_CACHE_HOME/shepherd/media/playlists/<list-id>.json` (falling back to
//! `~/.cache/…`). The cache is valid for [`CACHE_TTL_SECS`] seconds; a stale
//! or absent cache causes a fresh yt-dlp fetch and a new cache write. If the
//! live fetch fails (typically because the network is unreachable) but a
//! stale cache entry exists, the stale entry is returned so the launcher
//! keeps working offline.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use shepherd_media_core::YoutubePlaylistEntry;
use tracing::{debug, warn};
use url::Url;

/// Cached playlist metadata is considered fresh for this many seconds.
const CACHE_TTL_SECS: u64 = 6 * 3600;

/// The result of successfully fetching a YouTube playlist.
pub struct PlaylistInfo {
    /// Playlist display title from yt-dlp, if present.
    pub title: Option<String>,
    /// The `list=…` parameter value (YouTube's playlist ID).
    pub playlist_id: Option<String>,
    /// Videos in playlist order.
    pub entries: Vec<YoutubePlaylistEntry>,
}

// --- yt-dlp JSON deserialization ---

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

// --- On-disk playlist metadata cache ---

#[derive(Serialize, Deserialize)]
struct CachedPlaylist {
    /// Unix timestamp (seconds) when this entry was written.
    fetched_at: u64,
    title: Option<String>,
    playlist_id: Option<String>,
    entries: Vec<CachedEntry>,
}

/// Mirror of `YoutubePlaylistEntry` with serde derives. Kept separate from
/// the core type so that cache serialization concerns don't leak into the
/// platform-agnostic library.
#[derive(Serialize, Deserialize)]
struct CachedEntry {
    video_id: String,
    title: String,
    duration_seconds: Option<u64>,
    /// Stored as a plain string; `Url` round-trips cleanly.
    thumbnail_url: Option<String>,
}

impl CachedEntry {
    fn from_entry(e: &YoutubePlaylistEntry) -> Self {
        CachedEntry {
            video_id: e.video_id.clone(),
            title: e.title.clone(),
            duration_seconds: e.duration_seconds,
            thumbnail_url: e.thumbnail_url.as_ref().map(|u| u.to_string()),
        }
    }

    fn into_entry(self) -> YoutubePlaylistEntry {
        YoutubePlaylistEntry {
            video_id: self.video_id,
            title: self.title,
            duration_seconds: self.duration_seconds,
            thumbnail_url: self.thumbnail_url.and_then(|s| Url::parse(&s).ok()),
        }
    }
}

/// Derive a filesystem-safe cache file path from a YouTube playlist URL.
///
/// The `list=` query parameter is used as the key: it is stable, human-
/// readable, and unique per playlist. Returns `None` if the URL has no
/// `list=` parameter or the cache root cannot be determined.
fn playlist_cache_path(url: &str) -> Option<PathBuf> {
    let parsed = Url::parse(url).ok()?;
    let list_id: String = parsed
        .query_pairs()
        .find(|(k, _)| k == "list")
        .map(|(_, v)| v.into_owned())?;

    // YouTube playlist IDs are already [A-Za-z0-9_-], but restrict to be
    // safe on all filesystems. Truncate to 128 chars so the filename stays
    // well within PATH_MAX on any path prefix.
    let safe: String = list_id
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(128)
        .collect();

    let cache_home = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;

    Some(
        cache_home
            .join("shepherd")
            .join("media")
            .join("playlists")
            .join(format!("{safe}.json")),
    )
}

/// Freshness of an on-disk cache hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CacheFreshness {
    /// Cached entry is within [`CACHE_TTL_SECS`] of now.
    Fresh,
    /// Cached entry is older than [`CACHE_TTL_SECS`]; usable as an offline
    /// fallback when a live fetch fails.
    Stale,
}

/// Try to load playlist metadata from the on-disk cache.
///
/// Returns `None` if the cache file is absent, unreadable, or unparseable.
/// All errors are logged at `warn` level and treated as cache misses so the
/// caller can fall back to a live fetch. The returned [`CacheFreshness`]
/// lets the caller decide whether to trust the entry directly or only use
/// it as an offline fallback.
fn load_from_cache(url: &str) -> Option<(PlaylistInfo, CacheFreshness)> {
    let path = playlist_cache_path(url)?;
    let bytes = std::fs::read(&path).ok()?;
    let cached: CachedPlaylist = match serde_json::from_slice(&bytes) {
        Ok(c) => c,
        Err(e) => {
            warn!("ignoring corrupt playlist cache at {}: {e}", path.display());
            return None;
        }
    };

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let freshness = if now.saturating_sub(cached.fetched_at) >= CACHE_TTL_SECS {
        debug!("playlist cache stale for {url}");
        CacheFreshness::Stale
    } else {
        debug!("playlist cache hit for {url} ({})", path.display());
        CacheFreshness::Fresh
    };

    let info = PlaylistInfo {
        title: cached.title,
        playlist_id: cached.playlist_id,
        entries: cached
            .entries
            .into_iter()
            .map(CachedEntry::into_entry)
            .collect(),
    };
    Some((info, freshness))
}

/// Write playlist metadata to the on-disk cache.
///
/// Errors are logged at `warn` level and ignored — a cache miss on the next
/// launch is always safe.
fn save_to_cache(url: &str, info: &PlaylistInfo) {
    let Some(path) = playlist_cache_path(url) else {
        return;
    };
    let Some(parent) = path.parent() else {
        return;
    };

    if let Err(e) = std::fs::create_dir_all(parent) {
        warn!(
            "could not create playlist cache dir {}: {e}",
            parent.display()
        );
        return;
    }

    let cached = CachedPlaylist {
        fetched_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        title: info.title.clone(),
        playlist_id: info.playlist_id.clone(),
        entries: info.entries.iter().map(CachedEntry::from_entry).collect(),
    };

    match serde_json::to_vec_pretty(&cached) {
        Ok(bytes) => {
            if let Err(e) = std::fs::write(&path, bytes) {
                warn!("could not write playlist cache to {}: {e}", path.display());
            } else {
                debug!("wrote playlist cache to {}", path.display());
            }
        }
        Err(e) => warn!("could not serialize playlist cache: {e}"),
    }
}

/// Fetch a YouTube playlist by URL using `yt-dlp`.
///
/// Checks the on-disk cache first; only invokes `yt-dlp` on a cache miss or
/// when the cached entry has expired. A successful live fetch is written back
/// to the cache before returning. If the live fetch fails but a stale cache
/// entry exists, the stale entry is returned with a warning so the launcher
/// remains usable offline.
///
/// Returns an error string suitable for printing directly to stderr.
///
/// The caller is responsible for having `yt-dlp` installed; see
/// `docs/shepherd-media.md` for setup instructions.
pub fn fetch_playlist(url: &str) -> Result<PlaylistInfo, String> {
    let cached = load_from_cache(url);
    if let Some((info, CacheFreshness::Fresh)) = cached {
        return Ok(info);
    }
    let stale = cached.map(|(info, _)| info);

    match fetch_playlist_live(url) {
        Ok(info) => {
            save_to_cache(url, &info);
            Ok(info)
        }
        Err(e) => match stale {
            Some(info) => {
                warn!("live playlist fetch failed for {url}: {e}; using stale cache");
                Ok(info)
            }
            None => Err(e),
        },
    }
}

/// Run `yt-dlp` to fetch fresh playlist metadata, with no cache interaction.
fn fetch_playlist_live(url: &str) -> Result<PlaylistInfo, String> {
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
