//! Video file cache for remote library sources.
//!
//! Two caching modes are implemented:
//!
//! - **Option A** (`queue_all`): at browse launch, every remote item in the
//!   library is queued for background download.  The worker skips items if the
//!   cache is already at capacity so that existing cached content is not
//!   displaced just to pre-fetch something the user has not yet played.
//!
//! - **Option B** (`queue_after_play`): when a video finishes naturally (EOF),
//!   its source URL is queued for download so the *next* time it is selected
//!   it plays from the local cache.  Because the user just watched this item
//!   the worker will evict the least-recently-used file(s) after downloading
//!   to bring the cache back within the size cap.
//!
//! LRU order is maintained by updating the file's mtime on every cache hit
//! inside `CachingPlayer::play`.  Eviction sorts by mtime ascending (oldest
//! access first).
//!
//! The cap defaults to [`DEFAULT_MAX_CACHE_BYTES`] and can be overridden via
//! `SHEPHERD_MEDIA_VIDEO_CACHE_MAX_BYTES`.
//!
//! Cache files live in `$XDG_CACHE_HOME/shepherd/media/videos/`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, mpsc};
use std::time::SystemTime;

use filetime::FileTime;
use shepherd_media_core::resolver::resolve_source;
use shepherd_media_core::{ClassifiedUri, Library, PlayerError, PlayerEvent, PlayerHandle, Source};
use tracing::{debug, info, warn};

use crate::platform;

// ---------------------------------------------------------------------------
// Cache size cap
// ---------------------------------------------------------------------------

/// Default maximum total size of the video cache.
const DEFAULT_MAX_CACHE_BYTES: u64 = 10 * 1024 * 1024 * 1024; // 10 GiB

/// Environment variable that overrides [`DEFAULT_MAX_CACHE_BYTES`].
const MAX_BYTES_ENV: &str = "SHEPHERD_MEDIA_VIDEO_CACHE_MAX_BYTES";

fn max_cache_bytes() -> u64 {
    std::env::var(MAX_BYTES_ENV)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_MAX_CACHE_BYTES)
}

// ---------------------------------------------------------------------------
// VideoCache
// ---------------------------------------------------------------------------

pub struct VideoCache {
    cache_dir: PathBuf,
    download_tx: mpsc::Sender<DownloadRequest>,
}

struct DownloadRequest {
    item_id: String,
    url: String,
    kind: DownloadKind,
    /// When `true` the worker evicts LRU files after downloading so the cache
    /// stays within the size cap.  When `false` (background prefetch) the
    /// worker skips this request if the cache is already at capacity rather
    /// than displacing existing content.
    evict_after: bool,
}

enum DownloadKind {
    YouTube,
    Http,
}

impl VideoCache {
    /// Construct a `VideoCache`, creating the cache directory and spawning the
    /// background worker thread.  Returns `None` if the cache directory cannot
    /// be determined or created; in that case the caller should skip caching.
    pub fn new() -> Option<Arc<Self>> {
        let cache_dir = video_cache_dir()?;
        if let Err(e) = std::fs::create_dir_all(&cache_dir) {
            warn!(
                "could not create video cache dir {}: {e}",
                cache_dir.display()
            );
            return None;
        }

        let (tx, rx) = mpsc::channel::<DownloadRequest>();
        let dir = cache_dir.clone();
        let max_bytes = max_cache_bytes();
        std::thread::Builder::new()
            .name("video-cache-worker".into())
            .spawn(move || download_worker(dir, rx, max_bytes))
            .ok()?;

        Some(Arc::new(VideoCache {
            cache_dir,
            download_tx: tx,
        }))
    }

    /// Return the path to a fully-downloaded cached video for `item_id`, or
    /// `None` if no complete file is present.
    pub fn cached_path(&self, item_id: &str) -> Option<PathBuf> {
        find_cached_file(&self.cache_dir, item_id)
    }

    /// Queue a background prefetch for `source` (Option A).  The worker will
    /// skip this download if the cache is already at capacity so that existing
    /// cached content is not evicted for items the user has not yet watched.
    pub fn queue_prefetch(&self, item_id: &str, source: &Source) {
        self.enqueue(item_id, source, false);
    }

    /// Queue a download because `source` was just played to completion
    /// (Option B).  The worker will evict the LRU file(s) after downloading
    /// to keep the cache within the size cap.
    pub fn queue_after_play(&self, item_id: &str, source: &Source) {
        self.enqueue(item_id, source, true);
    }

    /// Queue background prefetch downloads for every remote item in `library`
    /// (Option A).
    pub fn queue_all(&self, library: &Library) {
        let platform_info = platform::current();
        for item in &library.items {
            if let Some(source) = resolve_source(item, &platform_info) {
                self.queue_prefetch(&item.id, source);
            }
        }
    }

    fn enqueue(&self, item_id: &str, source: &Source, evict_after: bool) {
        let (url, kind) = match &source.uri {
            ClassifiedUri::YouTube(url) => (url.to_string(), DownloadKind::YouTube),
            ClassifiedUri::DirectHttp(url) => (url.to_string(), DownloadKind::Http),
            _ => return,
        };

        if self.cached_path(item_id).is_some() {
            debug!("video cache hit for {item_id}, skipping queue");
            return;
        }

        let _ = self.download_tx.send(DownloadRequest {
            item_id: item_id.to_string(),
            url,
            kind,
            evict_after,
        });
    }
}

// ---------------------------------------------------------------------------
// CachingPlayer
// ---------------------------------------------------------------------------

/// A `PlayerHandle` wrapper that substitutes cached local files for remote
/// sources and queues downloads after natural playback completion (Option B).
pub struct CachingPlayer {
    inner: Box<dyn PlayerHandle>,
    cache: Arc<VideoCache>,
    /// Maps source URL strings to item IDs for reverse lookup in `play`.
    url_to_id: HashMap<String, String>,
    /// The (item_id, source) of the currently-playing remote item, for Option B.
    last_played: Option<(String, Source)>,
}

impl CachingPlayer {
    pub fn new(inner: Box<dyn PlayerHandle>, cache: Arc<VideoCache>, library: &Library) -> Self {
        let platform_info = platform::current();
        let mut url_to_id = HashMap::new();
        for item in &library.items {
            if let Some(source) = resolve_source(item, &platform_info)
                && let Some(url) = source_url(source)
            {
                url_to_id.insert(url, item.id.clone());
            }
        }
        Self {
            inner,
            cache,
            url_to_id,
            last_played: None,
        }
    }
}

impl PlayerHandle for CachingPlayer {
    fn play(&mut self, source: &Source) -> Result<(), PlayerError> {
        let url = source_url(source);
        let item_id = url.as_deref().and_then(|u| self.url_to_id.get(u)).cloned();
        let cached = item_id.as_deref().and_then(|id| self.cache.cached_path(id));

        if let Some(path) = cached {
            debug!("playing from video cache: {}", path.display());
            // Update mtime so LRU eviction keeps frequently-watched files warm.
            let _ = filetime::set_file_mtime(&path, FileTime::now());
            // Already cached — no need to re-download after EOF.
            self.last_played = None;
            let local = Source {
                platforms: source.platforms.clone(),
                uri: ClassifiedUri::Local(path),
                player_hint: source.player_hint,
            };
            self.inner.play(&local)
        } else {
            // Not cached; record for Option B download after EOF.
            self.last_played = item_id.map(|id| (id, source.clone()));
            self.inner.play(source)
        }
    }

    fn stop(&mut self) -> Result<(), PlayerError> {
        self.last_played = None;
        self.inner.stop()
    }

    fn is_playing(&self) -> bool {
        self.inner.is_playing()
    }

    fn poll_event(&mut self) -> Option<PlayerEvent> {
        let event = self.inner.poll_event()?;
        if matches!(event, PlayerEvent::EndOfFile)
            && let Some((ref item_id, ref source)) = self.last_played
        {
            // Option B: the user watched this to completion — cache it so the
            // next play comes from local storage, evicting LRU if needed.
            self.cache.queue_after_play(item_id, source);
        }
        Some(event)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn video_cache_dir() -> Option<PathBuf> {
    let cache_home = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(cache_home.join("shepherd").join("media").join("videos"))
}

/// Extract a URL string from a remote `Source`, returning `None` for local
/// paths that do not need downloading.
fn source_url(source: &Source) -> Option<String> {
    match &source.uri {
        ClassifiedUri::YouTube(url)
        | ClassifiedUri::DirectHttp(url)
        | ClassifiedUri::Unknown(url) => Some(url.to_string()),
        ClassifiedUri::Local(_) => None,
    }
}

/// Scan `cache_dir` for a completed download file named `<item_id>.<ext>`,
/// excluding in-progress `.part` files.  Returns the first match.
fn find_cached_file(cache_dir: &Path, item_id: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(cache_dir).ok()?;
    let prefix = format!("{item_id}.");
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with(&prefix) && !name_str.ends_with(".part") {
            return Some(entry.path());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// LRU eviction
// ---------------------------------------------------------------------------

struct CacheEntry {
    path: PathBuf,
    size: u64,
    mtime: SystemTime,
}

/// Collect all complete (non-`.part`) files in `cache_dir` with their sizes
/// and mtimes.  Returns `None` only if the directory cannot be read at all.
fn collect_cache_entries(cache_dir: &Path) -> Option<Vec<CacheEntry>> {
    let mut entries = Vec::new();
    for de in std::fs::read_dir(cache_dir).ok()?.flatten() {
        let name = de.file_name();
        let name_str = name.to_string_lossy();
        if name_str.ends_with(".part") {
            continue;
        }
        let Ok(meta) = de.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        entries.push(CacheEntry {
            path: de.path(),
            size: meta.len(),
            mtime,
        });
    }
    Some(entries)
}

fn cache_total(cache_dir: &Path) -> u64 {
    collect_cache_entries(cache_dir)
        .map(|e| e.iter().map(|x| x.size).sum())
        .unwrap_or(0)
}

/// Evict LRU files from `cache_dir` until the total size is at or below
/// `target_bytes`.  Errors on individual deletes are logged and skipped.
fn evict_to(cache_dir: &Path, target_bytes: u64) {
    let Some(mut entries) = collect_cache_entries(cache_dir) else {
        return;
    };

    let total: u64 = entries.iter().map(|e| e.size).sum();
    if total <= target_bytes {
        return;
    }

    // Oldest mtime first (least recently used).
    entries.sort_unstable_by_key(|e| e.mtime);

    let mut remaining = total;
    for entry in entries {
        if remaining <= target_bytes {
            break;
        }
        match std::fs::remove_file(&entry.path) {
            Ok(()) => {
                info!("evicted cached video: {}", entry.path.display());
                remaining = remaining.saturating_sub(entry.size);
            }
            Err(e) => warn!("cache eviction failed for {}: {e}", entry.path.display()),
        }
    }
}

// ---------------------------------------------------------------------------
// Background download worker
// ---------------------------------------------------------------------------

fn download_worker(cache_dir: PathBuf, rx: mpsc::Receiver<DownloadRequest>, max_bytes: u64) {
    for req in rx {
        if find_cached_file(&cache_dir, &req.item_id).is_some() {
            debug!("video already cached, skipping: {}", req.item_id);
            continue;
        }

        if !req.evict_after {
            // Background prefetch: do not displace existing cached content.
            // If the cache is already at capacity, leave this item for Option B
            // to handle when the user actually plays it.
            if cache_total(&cache_dir) >= max_bytes {
                debug!(
                    "cache at capacity, skipping background prefetch: {}",
                    req.item_id
                );
                continue;
            }
        }

        debug!("downloading video: {}", req.item_id);
        let result = match req.kind {
            DownloadKind::YouTube => download_youtube(&cache_dir, &req.item_id, &req.url),
            DownloadKind::Http => download_http(&cache_dir, &req.item_id, &req.url),
        };

        match result {
            Ok(()) => {
                debug!("cached video: {}", req.item_id);
                // Trim back to the cap.  For Option B downloads this is the
                // primary eviction path.  For Option A it handles the edge
                // case where a large file pushed us just over the cap.
                evict_to(&cache_dir, max_bytes);
            }
            Err(e) => warn!("failed to cache video {}: {e}", req.item_id),
        }
    }
}

fn download_youtube(cache_dir: &Path, item_id: &str, url: &str) -> Result<(), String> {
    let output_template = cache_dir.join(format!("{item_id}.%(ext)s"));
    let status = Command::new("yt-dlp")
        .args([
            "--quiet",
            "--no-warnings",
            "--format",
            "bestvideo[height<=?1080]+bestaudio/best[height<=?1080]/best",
            "--output",
        ])
        .arg(&output_template)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("failed to spawn yt-dlp: {e}"))?;

    if !status.success() {
        return Err(format!("yt-dlp exited with {status} for {url}"));
    }
    Ok(())
}

fn download_http(cache_dir: &Path, item_id: &str, url: &str) -> Result<(), String> {
    let ext = url
        .rsplit('.')
        .next()
        .filter(|s| s.len() <= 5 && s.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or("bin");

    let part_path = cache_dir.join(format!("{item_id}.part"));
    let final_path = cache_dir.join(format!("{item_id}.{ext}"));

    let resp = ureq::get(url)
        .call()
        .map_err(|e| format!("HTTP request failed for {url}: {e}"))?;

    let mut reader = resp.into_reader();
    let mut file = std::fs::File::create(&part_path)
        .map_err(|e| format!("failed to create part file: {e}"))?;

    std::io::copy(&mut reader, &mut file).map_err(|e| format!("failed to write download: {e}"))?;

    std::fs::rename(&part_path, &final_path)
        .map_err(|e| format!("failed to rename part file: {e}"))?;

    Ok(())
}
