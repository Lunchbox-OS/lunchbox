//! On-disk cache for remote video/audio sources, making the per-library cache
//! mode and size cap (from the settings UI) functional.
//!
//! Only `direct-http` sources are cached: `file://` is already local, and
//! YouTube needs yt-dlp to resolve a real media URL (not wired yet). Playback
//! consults the cache first; with a non-`Off` cache mode the just-watched item
//! is downloaded afterward so the next play is local. LRU eviction (by file
//! mtime, touched on each cache hit) keeps the directory within the cap.
//!
//! Download and eviction are blocking and run off the UI thread.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};

use filetime::FileTime;
use shepherd_media_app::lru::{self, LruEntry};

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

    /// Cache file path for a URL (stable hash + the URL's media extension).
    pub fn path_for(&self, url: &str) -> PathBuf {
        let mut h = DefaultHasher::new();
        url.hash(&mut h);
        let ext = media_extension(url).unwrap_or("bin");
        self.dir.join(format!("{:016x}.{ext}", h.finish()))
    }

    /// If a cached copy exists, mark it most-recently-used and return its path.
    pub fn cached_path(&self, url: &str) -> Option<PathBuf> {
        let path = self.path_for(url);
        if path.is_file() {
            let _ = filetime::set_file_mtime(&path, FileTime::now());
            Some(path)
        } else {
            None
        }
    }

    /// Download `url` into the cache (blocking) and evict LRU files to stay
    /// within the cap. A no-op returning the existing path if already cached.
    pub fn store(&self, url: &str) -> io::Result<PathBuf> {
        let path = self.path_for(url);
        if path.is_file() {
            let _ = filetime::set_file_mtime(&path, FileTime::now());
            return Ok(path);
        }
        std::fs::create_dir_all(&self.dir)?;
        let tmp = path.with_extension("part");
        download(url, &tmp)?;
        std::fs::rename(&tmp, &path)?;
        self.evict_to_cap();
        Ok(path)
    }

    /// Remove least-recently-used files until the total size is within the cap,
    /// via the shared LRU policy (see `shepherd_media_app::lru`).
    fn evict_to_cap(&self) {
        lru::evict_to_cap(self.entries(), self.max_bytes, |_| {});
    }

    /// All committed cache files (skipping in-flight `.part` downloads) as LRU
    /// entries keyed on file mtime.
    fn entries(&self) -> Vec<LruEntry<FileTime>> {
        let Ok(rd) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        rd.flatten()
            .filter_map(|entry| {
                let path = entry.path();
                let meta = entry.metadata().ok()?;
                if !meta.is_file() {
                    return None;
                }
                // Skip in-flight downloads.
                if path.extension().and_then(|e| e.to_str()) == Some("part") {
                    return None;
                }
                Some(LruEntry {
                    size: meta.len(),
                    recency: FileTime::from_last_modification_time(&meta),
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
    fn evicts_oldest_until_within_cap() {
        // Cap 250 bytes; three 100-byte files -> oldest must be evicted.
        let cache = VideoCache::new(unique_dir("evict"), 250);
        seed(&cache, "https://x/old.mp4", 100, 1_000); // oldest
        seed(&cache, "https://x/mid.mp4", 100, 2_000);
        seed(&cache, "https://x/new.mp4", 100, 3_000); // newest
        cache.evict_to_cap();

        assert!(
            !cache.path_for("https://x/old.mp4").exists(),
            "oldest evicted"
        );
        assert!(cache.path_for("https://x/mid.mp4").exists());
        assert!(cache.path_for("https://x/new.mp4").exists());
        let total: u64 = cache.entries().iter().map(|e| e.size).sum();
        assert!(total <= 250);
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
