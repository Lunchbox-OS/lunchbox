//! On-disk cache for remote video/audio sources, making the per-library cache
//! mode and size cap (from the settings UI) functional.
//!
//! Only `direct-http` sources are cached: `file://` is already local, and
//! YouTube needs yt-dlp to resolve a real media URL (not wired yet). Playback
//! consults the cache first; with a non-`Off` cache mode the just-watched item
//! is downloaded afterward so the next play is local.
//!
//! Eviction policy is shared with the Linux cache (`shepherd-media-cache`)
//! through `shepherd-media-app`: files nobody has watched are spent before any
//! file somebody did, see [`shepherd_media_app::Recency`]. That matters once
//! the `All` cache mode actually prefetches, which it does not yet.
//!
//! Download and eviction are blocking and run off the UI thread.

use std::io;
use std::path::{Path, PathBuf};

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use shepherd_media_app::interest;
use shepherd_media_app::lru::{self, LruEntry, Recency};

/// A per-library video cache rooted at `dir` with a `max_bytes` budget.
#[derive(Clone)]
pub struct VideoCache {
    dir: PathBuf,
    max_bytes: u64,
}

impl VideoCache {
    pub fn new(dir: PathBuf, max_bytes: u64) -> Self {
        Self { dir, max_bytes }
    }

    /// The cache key for a URL. One rendition per URL, so this doubles as the
    /// key for the played marker.
    fn key_for(url: &str) -> String {
        let mut h = DefaultHasher::new();
        url.hash(&mut h);
        format!("{:016x}", h.finish())
    }

    /// Cache file path for a URL (stable hash + the URL's media extension).
    pub fn path_for(&self, url: &str) -> PathBuf {
        let ext = media_extension(url).unwrap_or("bin");
        self.dir.join(format!("{}.{ext}", Self::key_for(url)))
    }

    /// If a cached copy exists, return its path.
    ///
    /// A pure lookup: it records nothing. Call [`Self::mark_played`] when
    /// playback actually starts, so that inspecting the cache cannot make a
    /// speculative download look watched.
    pub fn cached_path(&self, url: &str) -> Option<PathBuf> {
        let path = self.path_for(url);
        path.is_file().then_some(path)
    }

    /// Record that `url` was played, so eviction stops treating it as a
    /// replaceable guess.
    pub fn mark_played(&self, url: &str) {
        if std::fs::create_dir_all(&self.dir).is_ok()
            && let Err(e) = interest::mark_played(&self.dir, &Self::key_for(url))
        {
            log::warn!("could not record playback of a cached video: {e}");
        }
    }

    /// Download `url` into the cache (blocking) and evict LRU files to stay
    /// within the cap. A no-op returning the existing path if already cached.
    pub fn store(&self, url: &str) -> io::Result<PathBuf> {
        let path = self.path_for(url);
        if path.is_file() {
            return Ok(path);
        }
        std::fs::create_dir_all(&self.dir)?;
        let tmp = path.with_extension("part");
        download(url, &tmp)?;
        std::fs::rename(&tmp, &path)?;
        // This download exists because the item was just watched. Recording
        // that before evicting is what stops it being the first thing the trim
        // below discards — it would otherwise arrive unwatched and evict
        // itself.
        self.mark_played(url);
        self.evict_to_cap();
        Ok(path)
    }

    /// Remove least-recently-used files until the total size is within the cap,
    /// via the shared LRU policy (see `shepherd_media_app::lru`).
    fn evict_to_cap(&self) {
        lru::evict_to_cap(self.entries(), self.max_bytes, |_| {});
    }

    /// All committed cache files as LRU entries, classified by whether anyone
    /// has watched them. In-flight downloads and played markers are skipped:
    /// neither is cached content, so neither counts toward the cap nor is
    /// eligible for eviction.
    fn entries(&self) -> Vec<LruEntry<Recency>> {
        let Ok(rd) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        rd.flatten()
            .filter_map(|entry| {
                let path = entry.path();
                let name = entry.file_name();
                let name = name.to_string_lossy();
                let meta = entry.metadata().ok()?;
                if !meta.is_file() {
                    return None;
                }
                if name.ends_with(".part") || interest::is_marker(&name) {
                    return None;
                }
                let key = path.file_stem()?.to_str()?;
                let downloaded_at = meta.modified().ok()?;
                Some(LruEntry {
                    size: meta.len(),
                    recency: Recency::classify(downloaded_at, interest::played_at(&self.dir, key)),
                    path,
                })
            })
            .collect()
    }
}

fn download(url: &str, dest: &Path) -> io::Result<()> {
    let resp = ureq::get(url)
        .call()
        .map_err(|e| io::Error::other(e.to_string()))?;
    let mut reader = resp.into_reader();
    let mut file = std::fs::File::create(dest)?;
    io::copy(&mut reader, &mut file)?;
    Ok(())
}

/// Extract a known media extension from a URL path, lowercased.
fn media_extension(url: &str) -> Option<&str> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let ext = path.rsplit('.').next()?;
    const KNOWN: &[&str] = &[
        "mp4", "mkv", "webm", "mov", "m4v", "mp3", "flac", "opus", "ogg", "m4a", "wav",
    ];
    KNOWN.iter().copied().find(|k| k.eq_ignore_ascii_case(ext))
}

#[cfg(test)]
mod tests {
    use super::*;
    use filetime::FileTime;

    fn unique_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("shepherd-media-vcache-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn seed(cache: &VideoCache, url: &str, size: usize, mtime_secs: i64) {
        std::fs::create_dir_all(&cache.dir).unwrap();
        let path = cache.path_for(url);
        std::fs::write(&path, vec![0u8; size]).unwrap();
        filetime::set_file_mtime(&path, FileTime::from_unix_time(mtime_secs, 0)).unwrap();
    }

    #[test]
    fn extension_detection() {
        assert_eq!(
            media_extension("https://x/y/video.MP4?token=1"),
            Some("mp4")
        );
        assert_eq!(media_extension("https://x/y/song.flac"), Some("flac"));
        assert_eq!(media_extension("https://x/y/nope.txt"), None);
        assert_eq!(media_extension("https://x/y/noext"), None);
    }

    #[test]
    fn path_is_stable_and_keeps_extension() {
        let cache = VideoCache::new(unique_dir("path"), 1_000);
        let p1 = cache.path_for("https://x/clip.mp4");
        let p2 = cache.path_for("https://x/clip.mp4");
        assert_eq!(p1, p2);
        assert_eq!(p1.extension().unwrap(), "mp4");
    }

    #[test]
    fn cached_path_hits_and_misses() {
        let cache = VideoCache::new(unique_dir("hit"), 1_000_000);
        let url = "https://x/clip.mp4";
        assert!(cache.cached_path(url).is_none());
        seed(&cache, url, 10, 1_000);
        assert!(cache.cached_path(url).is_some());
    }

    #[test]
    fn among_unwatched_files_the_newest_download_is_evicted_first() {
        // Cap 250 bytes; three unwatched 100-byte files. The newest goes:
        // a cache fills in the order items are listed, so the newest arrival is
        // the furthest down the list and the least likely to be reached next.
        let cache = VideoCache::new(unique_dir("evict"), 250);
        seed(&cache, "https://x/old.mp4", 100, 1_000);
        seed(&cache, "https://x/mid.mp4", 100, 2_000);
        seed(&cache, "https://x/new.mp4", 100, 3_000); // newest
        cache.evict_to_cap();

        assert!(
            !cache.path_for("https://x/new.mp4").exists(),
            "the newest guess is evicted"
        );
        assert!(cache.path_for("https://x/old.mp4").exists());
        assert!(cache.path_for("https://x/mid.mp4").exists());
        let total: u64 = cache.entries().iter().map(|e| e.size).sum();
        assert!(total <= 250);
    }

    #[test]
    fn a_watched_file_survives_every_unwatched_one() {
        // The headline of the shared ordering: something the user chose to
        // watch outranks a speculative download however recent.
        let cache = VideoCache::new(unique_dir("watched"), 100);
        seed(&cache, "https://x/watched.mp4", 100, 1_000); // oldest by mtime
        seed(&cache, "https://x/guess.mp4", 100, 3_000);
        cache.mark_played("https://x/watched.mp4");

        cache.evict_to_cap();

        assert!(
            cache.path_for("https://x/watched.mp4").exists(),
            "a watched video must not be displaced by a guess"
        );
        assert!(!cache.path_for("https://x/guess.mp4").exists());
    }

    #[test]
    fn a_lookup_does_not_count_as_a_play() {
        let cache = VideoCache::new(unique_dir("lookup"), 1_000);
        let url = "https://x/clip.mp4";
        seed(&cache, url, 10, 1_000);

        let _ = cache.cached_path(url);
        assert!(
            cache.entries()[0].recency.is_unwatched(),
            "inspecting the cache must not make a guess look watched"
        );

        cache.mark_played(url);
        assert!(!cache.entries()[0].recency.is_unwatched());
    }

    #[test]
    fn a_played_marker_is_not_cached_content() {
        let cache = VideoCache::new(unique_dir("marker"), 1_000);
        let url = "https://x/clip.mp4";
        seed(&cache, url, 10, 1_000);
        cache.mark_played(url);

        let entries = cache.entries();
        assert_eq!(entries.len(), 1, "the marker must not be counted as a file");
        assert_eq!(entries[0].size, 10);
    }

    #[test]
    fn evict_is_noop_within_cap() {
        let cache = VideoCache::new(unique_dir("noop"), 1_000);
        seed(&cache, "https://x/a.mp4", 100, 1_000);
        seed(&cache, "https://x/b.mp4", 100, 2_000);
        cache.evict_to_cap();
        assert!(cache.path_for("https://x/a.mp4").exists());
        assert!(cache.path_for("https://x/b.mp4").exists());
    }
}
