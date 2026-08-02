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
use std::ffi::{CStr, c_void};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, mpsc};
use std::time::SystemTime;

use filetime::FileTime;
use shepherd_media_app::lru::{self, LruEntry};
use shepherd_media_core::resolver::resolve_source;
use shepherd_media_core::{ClassifiedUri, Library, PlayerError, PlayerEvent, PlayerHandle, Source};
use tracing::{debug, info, warn};

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
    /// The yt-dlp selector downloads use, kept so `enqueue` can tell a file
    /// downloaded under the current selector from one that predates it.
    ytdl_format: String,
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

impl DownloadKind {
    /// The format selector recorded in (and compared against) the done
    /// sentinel. A direct HTTP download picks no format, so it records none.
    fn selector<'a>(&self, ytdl_format: &'a str) -> &'a str {
        match self {
            DownloadKind::YouTube => ytdl_format,
            DownloadKind::Http => "",
        }
    }
}

impl VideoCache {
    /// Construct a `VideoCache`, creating the cache directory and spawning the
    /// background worker thread.  Returns `None` if the cache directory cannot
    /// be determined or created; in that case the caller should skip caching.
    pub fn new(ytdl_format: &str) -> Option<Arc<Self>> {
        let cache_dir = crate::paths::media_cache_dir("videos")?;
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
        let format = ytdl_format.to_string();
        std::thread::Builder::new()
            .name("video-cache-worker".into())
            .spawn(move || download_worker(dir, rx, max_bytes, format))
            .ok()?;

        Some(Arc::new(VideoCache {
            cache_dir,
            download_tx: tx,
            ytdl_format: ytdl_format.to_string(),
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
        let platform_info = shepherd_media_core::PlatformInfo::current();
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

        match cache_state(&self.cache_dir, item_id, kind.selector(&self.ytdl_format)) {
            CacheState::Fresh => {
                debug!("video cache hit for {item_id}, skipping queue");
                return;
            }
            CacheState::StaleSelector => {
                debug!("cached {item_id} predates the current format selector, re-downloading");
            }
            CacheState::Absent => {}
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
        let platform_info = shepherd_media_core::PlatformInfo::current();
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

    fn set_paused(&mut self, paused: bool) -> Result<(), PlayerError> {
        self.inner.set_paused(paused)
    }

    fn is_paused(&self) -> bool {
        self.inner.is_paused()
    }

    fn seek_relative(&mut self, delta_seconds: f64) -> Result<(), PlayerError> {
        self.inner.seek_relative(delta_seconds)
    }

    fn seek_absolute(&mut self, seconds: f64) -> Result<(), PlayerError> {
        self.inner.seek_absolute(seconds)
    }

    fn position(&self) -> Option<f64> {
        self.inner.position()
    }

    fn duration(&self) -> Option<f64> {
        self.inner.duration()
    }

    fn set_volume(&mut self, percent: f64) -> Result<(), PlayerError> {
        self.inner.set_volume(percent)
    }

    fn volume(&self) -> Option<f64> {
        self.inner.volume()
    }

    fn set_start_position(&mut self, seconds: Option<f64>) {
        self.inner.set_start_position(seconds);
    }

    fn bind_gl(
        &mut self,
        get_proc_address: &dyn Fn(&CStr) -> *const c_void,
        native_display: Option<shepherd_media_core::NativeDisplay>,
    ) -> Result<(), PlayerError> {
        self.inner.bind_gl(get_proc_address, native_display)
    }

    fn render(&self, fbo: i32, width: i32, height: i32) -> Result<(), PlayerError> {
        self.inner.render(fbo, width, height)
    }

    fn set_redraw_callback(&mut self, cb: Box<dyn Fn() + Send + Sync + 'static>) {
        self.inner.set_redraw_callback(cb);
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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

/// Scan `cache_dir` for a completed download file named `<item_id>.<ext>`.
///
/// Requires the sentinel file `<item_id>.done` to be present; without it the
/// download is considered in-progress (yt-dlp may have written intermediate
/// per-format files that are not yet merged) and `None` is returned.
fn find_cached_file(cache_dir: &Path, item_id: &str) -> Option<PathBuf> {
    // The sentinel is written only after the download fully commits.
    if !cache_dir.join(format!("{item_id}.done")).exists() {
        return None;
    }
    let prefix = format!("{item_id}.");
    for entry in std::fs::read_dir(cache_dir).ok()?.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with(&prefix)
            && !name_str.ends_with(".part")
            && !name_str.ends_with(".done")
        {
            return Some(entry.path());
        }
    }
    None
}

/// Write the completion sentinel for `item_id`.  Called once the video file
/// is fully on disk and ready to play.
///
/// The sentinel carries the yt-dlp format selector the file was downloaded
/// with, so a later change to that selector can be detected — see
/// [`cache_state`].  Direct-HTTP downloads involve no selector and record an
/// empty one.
fn write_done_sentinel(cache_dir: &Path, item_id: &str, selector: &str) -> Result<(), String> {
    let path = cache_dir.join(format!("{item_id}.done"));
    std::fs::write(&path, selector.as_bytes())
        .map_err(|e| format!("failed to write done sentinel: {e}"))
}

/// What the cache holds for an item, relative to the selector we would download
/// it with today.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CacheState {
    /// Nothing committed for this item.
    Absent,
    /// A committed file downloaded with the selector currently in force.
    Fresh,
    /// A committed file downloaded with a *different* selector.
    ///
    /// It still plays, so `cached_path` keeps handing it out — an offline device
    /// must not lose content it already has.  But it is the wrong codec: when
    /// the selector changed to prefer H.264 for hardware decoding (issue #115),
    /// every already-cached VP9 file would otherwise keep costing ~5x the CPU
    /// forever.  So a queue request replaces it.
    StaleSelector,
}

/// Classify what the cache holds for `item_id` against `selector`.
fn cache_state(cache_dir: &Path, item_id: &str, selector: &str) -> CacheState {
    if find_cached_file(cache_dir, item_id).is_none() {
        return CacheState::Absent;
    }
    // A sentinel written before this field existed reads as empty, which
    // differs from any YouTube selector (so those refresh once) and matches the
    // empty selector recorded for direct-HTTP files (so those don't).
    let recorded = std::fs::read_to_string(cache_dir.join(format!("{item_id}.done")))
        .unwrap_or_default()
        .trim()
        .to_string();
    if recorded == selector {
        CacheState::Fresh
    } else {
        CacheState::StaleSelector
    }
}

/// Delete every committed file for `item_id` plus its sentinel.
///
/// Called before re-downloading a [`CacheState::StaleSelector`] item: the new
/// download may land on a different extension (`.webm` → `.mp4`), and
/// `find_cached_file` returns whichever of the two `read_dir` yields first, so
/// leaving both behind would make playback pick a codec at random.
fn remove_cached_item(cache_dir: &Path, item_id: &str) {
    let prefix = format!("{item_id}.");
    let Ok(entries) = std::fs::read_dir(cache_dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(&prefix)
            && let Err(e) = std::fs::remove_file(entry.path())
        {
            warn!("could not remove stale cache file {:?}: {e}", entry.path());
        }
    }
}

// ---------------------------------------------------------------------------
// LRU eviction
// ---------------------------------------------------------------------------

struct CacheEntry {
    path: PathBuf,
    size: u64,
    mtime: SystemTime,
}

/// Collect all committed video files in `cache_dir` — those that have a
/// corresponding `<item_id>.done` sentinel — with their sizes and mtimes.
/// In-progress downloads (no sentinel) are excluded so they do not count
/// toward the size cap or get evicted mid-download.
/// Returns `None` only if the directory cannot be read at all.
fn collect_cache_entries(cache_dir: &Path) -> Option<Vec<CacheEntry>> {
    let mut entries = Vec::new();
    for de in std::fs::read_dir(cache_dir).ok()?.flatten() {
        let name = de.file_name();
        let name_str = name.to_string_lossy();
        if name_str.ends_with(".part") || name_str.ends_with(".done") {
            continue;
        }
        let Ok(meta) = de.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        // Derive item_id from the filename stem (e.g. "my-video" from "my-video.mp4").
        let item_id = match de.path().file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        // Only include files whose download has been fully committed.
        if !cache_dir.join(format!("{item_id}.done")).exists() {
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
/// `target_bytes`, via the shared LRU policy (see `shepherd_media_app::lru`).
/// On each eviction the paired `.done` sentinel is removed too, so
/// `find_cached_file` won't return a stale hit for the deleted video.
fn evict_to(cache_dir: &Path, target_bytes: u64) {
    let Some(entries) = collect_cache_entries(cache_dir) else {
        return;
    };
    let entries: Vec<LruEntry<SystemTime>> = entries
        .into_iter()
        .map(|e| LruEntry {
            path: e.path,
            size: e.size,
            recency: e.mtime,
        })
        .collect();

    lru::evict_to_cap(entries, target_bytes, |path| {
        info!("evicted cached video: {}", path.display());
        if let Some(item_id) = path.file_stem().and_then(|s| s.to_str()) {
            let _ = std::fs::remove_file(cache_dir.join(format!("{item_id}.done")));
        }
    });
}

// ---------------------------------------------------------------------------
// Background download worker
// ---------------------------------------------------------------------------

fn download_worker(
    cache_dir: PathBuf,
    rx: mpsc::Receiver<DownloadRequest>,
    max_bytes: u64,
    ytdl_format: String,
) {
    for req in rx {
        match cache_state(&cache_dir, &req.item_id, req.kind.selector(&ytdl_format)) {
            CacheState::Fresh => {
                debug!("video already cached, skipping: {}", req.item_id);
                continue;
            }
            // Clear the old file first: the replacement may land on a different
            // extension, and two files for one item make playback ambiguous.
            CacheState::StaleSelector => remove_cached_item(&cache_dir, &req.item_id),
            CacheState::Absent => {}
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
            DownloadKind::YouTube => {
                download_youtube(&cache_dir, &req.item_id, &req.url, &ytdl_format)
            }
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

fn download_youtube(
    cache_dir: &Path,
    item_id: &str,
    url: &str,
    ytdl_format: &str,
) -> Result<(), String> {
    let output_template = cache_dir.join(format!("{item_id}.%(ext)s"));
    let status = Command::new("yt-dlp")
        .args([
            "--quiet",
            "--no-warnings",
            // Match playback's player clients so DRM-protected uploads download
            // their progressive itag-18 fallback instead of failing.
            "--extractor-args",
            shepherd_media_core::YOUTUBE_EXTRACTOR_ARGS,
            "--format",
            ytdl_format,
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
    write_done_sentinel(cache_dir, item_id, ytdl_format)
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

    // No format selector is involved in a direct download.
    write_done_sentinel(cache_dir, item_id, "")
}

#[cfg(test)]
mod cache_state_tests {
    use super::*;

    const H264: &str = "bv*[vcodec^=avc1][height<=?1080]+ba/b";
    const ANY: &str = "bestvideo[height<=?1080]+bestaudio/best";

    /// Commit `item_id` to the cache as if downloaded with `selector`.
    fn commit(dir: &Path, item_id: &str, ext: &str, selector: &str) {
        std::fs::write(dir.join(format!("{item_id}.{ext}")), b"video").unwrap();
        write_done_sentinel(dir, item_id, selector).unwrap();
    }

    #[test]
    fn absent_when_nothing_committed() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(cache_state(dir.path(), "clip", H264), CacheState::Absent);
    }

    #[test]
    fn absent_while_a_download_is_still_in_flight() {
        // A file with no sentinel is an unfinished download, not a stale one —
        // treating it as stale would delete a partial file out from under the
        // worker that is writing it.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("clip.part"), b"partial").unwrap();
        assert_eq!(cache_state(dir.path(), "clip", H264), CacheState::Absent);
    }

    #[test]
    fn fresh_when_the_selector_matches() {
        let dir = tempfile::tempdir().unwrap();
        commit(dir.path(), "clip", "mp4", H264);
        assert_eq!(cache_state(dir.path(), "clip", H264), CacheState::Fresh);
    }

    #[test]
    fn stale_when_the_selector_changed() {
        let dir = tempfile::tempdir().unwrap();
        commit(dir.path(), "clip", "webm", ANY);
        assert_eq!(
            cache_state(dir.path(), "clip", H264),
            CacheState::StaleSelector,
            "a file downloaded under the old any-codec selector must be refreshed"
        );
    }

    #[test]
    fn sentinels_written_before_this_field_existed_refresh_once() {
        // Upgrading from a build whose sentinel was an empty marker.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("clip.webm"), b"video").unwrap();
        std::fs::write(dir.path().join("clip.done"), b"").unwrap();
        assert_eq!(
            cache_state(dir.path(), "clip", H264),
            CacheState::StaleSelector
        );
    }

    #[test]
    fn direct_http_downloads_stay_fresh_across_selector_changes() {
        // They never involved a selector, so changing it must not re-download
        // every plain-HTTP video in the library.
        let dir = tempfile::tempdir().unwrap();
        commit(dir.path(), "clip", "mp4", DownloadKind::Http.selector(ANY));
        assert_eq!(
            cache_state(dir.path(), "clip", DownloadKind::Http.selector(H264)),
            CacheState::Fresh
        );
    }

    #[test]
    fn stale_files_are_still_playable_until_replaced() {
        // `cached_path` deliberately keeps serving a stale file so an offline
        // device does not lose content it already has.
        let dir = tempfile::tempdir().unwrap();
        commit(dir.path(), "clip", "webm", ANY);
        assert!(find_cached_file(dir.path(), "clip").is_some());
    }

    #[test]
    fn removing_an_item_clears_every_extension_and_the_sentinel() {
        let dir = tempfile::tempdir().unwrap();
        commit(dir.path(), "clip", "webm", ANY);
        std::fs::write(dir.path().join("clip.part"), b"partial").unwrap();
        // A different item must survive.
        commit(dir.path(), "other", "mp4", H264);

        remove_cached_item(dir.path(), "clip");

        assert_eq!(cache_state(dir.path(), "clip", ANY), CacheState::Absent);
        assert!(!dir.path().join("clip.part").exists());
        assert_eq!(cache_state(dir.path(), "other", H264), CacheState::Fresh);
    }

    #[test]
    fn a_replacement_download_does_not_leave_two_codecs_behind() {
        // The regression this guards: .webm and .mp4 both present, with
        // `find_cached_file` picking whichever the directory listing yields.
        let dir = tempfile::tempdir().unwrap();
        commit(dir.path(), "clip", "webm", ANY);
        remove_cached_item(dir.path(), "clip");
        commit(dir.path(), "clip", "mp4", H264);

        let cached = find_cached_file(dir.path(), "clip").expect("replacement is cached");
        assert_eq!(cached.extension().unwrap(), "mp4");
        assert_eq!(cache_state(dir.path(), "clip", H264), CacheState::Fresh);
    }
}
