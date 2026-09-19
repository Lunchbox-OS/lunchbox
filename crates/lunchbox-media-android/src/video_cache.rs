//! On-disk cache for remote video/audio sources, making the per-library cache
//! mode and size cap (from the settings UI) functional.
//!
//! Only `direct-http` sources are cached: `file://` is already local, and
//! YouTube needs yt-dlp to resolve a real media URL (not wired yet). Playback
//! consults the cache first; with a non-`Off` cache mode the just-watched item
//! is downloaded afterward so the next play is local.
//!
//! Naming and eviction policy are shared with the Linux cache
//! (`lunchbox-media-cache`) through `lunchbox-media-app`, so the two behave the
//! same way where they can. Files are named by
//! [`lunchbox_media_app::content_key`] — a truncated SHA-256, *not* the
//! `DefaultHasher` this used to use, whose output is unspecified across Rust
//! releases and would rename the whole cache on a toolchain bump. Direct HTTP
//! involves no format selection, so the selector is empty here.
//!
//! Files written under the old naming are orphaned by that change — nothing
//! will ask for them again — and are left in place: they are ordinary eviction
//! candidates that age out on their own, and re-downloading them is what the
//! next toolchain bump would have cost anyway.
//!
//! Eviction scores each file by what it is worth and spends the cheapest first;
//! see [`lunchbox_media_app::Score`]. Watching something buys it a grace period
//! against being displaced, which erodes with time, so a file watched once long
//! ago eventually yields to newer content rather than holding its place
//! forever. That matters once the `All` cache mode actually prefetches, which
//! it does not yet — until then every file here is either watched or a leftover.
//!
//! Two of the three inputs the shared policy takes are degenerate here. There
//! is no library ordinal, because nothing prefetches in display order, so every
//! file scores as the tail of a list nothing is walking. And with one rendition
//! per URL, the content key doubles as the interest key both markers hang off.
//!
//! Download and eviction are blocking and run off the UI thread.

use std::io;
use std::path::{Path, PathBuf};

use lunchbox_media_app::content_key;
use lunchbox_media_app::interest;
use lunchbox_media_app::lru::{self, LruEntry, Score, ScoreWeights, Standing};

/// A per-library video cache rooted at `dir` with a `max_bytes` budget.
#[derive(Clone)]
pub struct VideoCache {
    dir: PathBuf,
    max_bytes: u64,
    weights: ScoreWeights,
}

impl VideoCache {
    pub fn new(dir: PathBuf, max_bytes: u64) -> Self {
        Self {
            dir,
            max_bytes,
            // There is no per-library caching setting for this yet, and the
            // shared default is the policy the Linux side ships with.
            weights: ScoreWeights::default(),
        }
    }

    /// The cache key for a URL. Direct HTTP picks no format, so the selector
    /// is empty; one rendition per URL means this doubles as the key for the
    /// played marker.
    fn key_for(url: &str) -> String {
        content_key(url, "")
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

    /// Record that `url` has been offered, so eviction can tell a recent
    /// addition from something that has been sitting here unwatched.
    ///
    /// Write-once — see [`interest::mark_seen`].
    pub fn mark_seen(&self, url: &str) {
        if std::fs::create_dir_all(&self.dir).is_ok()
            && let Err(e) = interest::mark_seen(&self.dir, &Self::key_for(url))
        {
            log::warn!("could not record first sighting of a cached video: {e}");
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
        self.mark_seen(url);
        // This download exists because the item was just watched. Recording
        // that before evicting is what stops it being the first thing the trim
        // below discards — it would otherwise arrive unwatched and evict
        // itself.
        self.mark_played(url);
        self.evict_to_cap();
        Ok(path)
    }

    /// Remove least-recently-used files until the total size is within the cap,
    /// via the shared LRU policy (see `lunchbox_media_app::lru`).
    fn evict_to_cap(&self) {
        lru::evict_to_cap(self.entries(), self.max_bytes, |_| {});
    }

    /// All committed cache files as LRU entries, scored by what each is worth.
    /// In-flight downloads and markers are skipped: neither is cached content,
    /// so neither counts toward the cap nor is eligible for eviction.
    fn entries(&self) -> Vec<LruEntry<Score>> {
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
                // The marker wins, but the file's own mtime is a floor: a cache
                // that predates the marker gets stamped on first use, and
                // taking that at face value would present all of it as newly
                // added.
                let first_seen = interest::first_seen_at(&self.dir, key)
                    .map_or(downloaded_at, |seen| seen.min(downloaded_at));
                let standing = Standing {
                    first_seen,
                    played_at: interest::played_at(&self.dir, key),
                    // Nothing here prefetches in library order, so no file has
                    // a position for the others to be measured against.
                    ordinal: None,
                };
                Some(LruEntry {
                    size: meta.len(),
                    score: Score::of(&standing, self.weights),
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
    fn among_unwatched_files_the_oldest_arrival_is_evicted_first() {
        // Cap 250 bytes; three unwatched 100-byte files. Nothing here prefetches
        // in library order, so no file has a position to be judged on and age
        // is all that separates them.
        let cache = VideoCache::new(unique_dir("evict"), 250);
        seed(&cache, "https://x/old.mp4", 100, 1_000); // oldest
        seed(&cache, "https://x/mid.mp4", 100, 2_000);
        seed(&cache, "https://x/new.mp4", 100, 3_000);
        cache.evict_to_cap();

        assert!(
            !cache.path_for("https://x/old.mp4").exists(),
            "the oldest arrival is evicted"
        );
        assert!(cache.path_for("https://x/mid.mp4").exists());
        assert!(cache.path_for("https://x/new.mp4").exists());
        let total: u64 = cache.entries().iter().map(|e| e.size).sum();
        assert!(total <= 250);
    }

    #[test]
    fn a_recently_watched_file_survives_every_unwatched_one() {
        // Something the user chose to watch outranks a speculative download,
        // however recent, for as long as its grace lasts.
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
    fn a_play_old_enough_to_have_lost_its_grace_stops_protecting_the_file() {
        // The other half of the shared policy: protection erodes, so a file
        // watched once long ago does not hold its place against newer content
        // forever.
        let cache = VideoCache::new(unique_dir("expired"), 100);
        let stale = "https://x/stale.mp4";
        let recent = "https://x/recent.mp4";
        // Both arrivals are dated relative to now, not to the epoch: the point
        // here is a grace measured in days, so the ages have to be real ones.
        seed(&cache, stale, 100, FileTime::now().unix_seconds());
        seed(&cache, recent, 100, FileTime::now().unix_seconds());
        cache.mark_played(stale);
        // Push the play well past the default 30-day grace.
        filetime::set_file_mtime(
            interest::marker_path(&cache.dir, &VideoCache::key_for(stale)),
            FileTime::from_unix_time(FileTime::now().unix_seconds() - 90 * 24 * 60 * 60, 0),
        )
        .unwrap();

        cache.evict_to_cap();

        assert!(!cache.path_for(stale).exists());
        assert!(cache.path_for(recent).exists());
    }

    #[test]
    fn a_re_download_is_not_a_fresh_arrival() {
        // The first-seen marker outlives the file, so an item that has churned
        // through the cache keeps competing on its real age.
        let cache = VideoCache::new(unique_dir("reseen"), 1_000);
        let url = "https://x/clip.mp4";
        cache.mark_seen(url);
        filetime::set_file_mtime(
            interest::seen_path(&cache.dir, &VideoCache::key_for(url)),
            FileTime::from_unix_time(1_000, 0),
        )
        .unwrap();
        // The file itself is written fresh, as a re-download would be.
        seed(&cache, url, 10, 9_000);

        let older = VideoCache::new(unique_dir("reseen-2"), 1_000);
        seed(&older, url, 10, 9_000);

        assert!(
            cache.entries()[0].score < older.entries()[0].score,
            "the marker, not the file, says how old the item is"
        );
    }

    #[test]
    fn a_lookup_does_not_count_as_a_play() {
        let cache = VideoCache::new(unique_dir("lookup"), 1_000);
        let url = "https://x/clip.mp4";
        seed(&cache, url, 10, 1_000);

        let _ = cache.cached_path(url);
        let before = cache.entries()[0].score;

        cache.mark_played(url);
        assert!(
            cache.entries()[0].score > before,
            "inspecting the cache must not count as a play; marking one must"
        );
    }

    #[test]
    fn markers_are_not_cached_content() {
        let cache = VideoCache::new(unique_dir("marker"), 1_000);
        let url = "https://x/clip.mp4";
        seed(&cache, url, 10, 1_000);
        cache.mark_played(url);
        cache.mark_seen(url);

        let entries = cache.entries();
        assert_eq!(entries.len(), 1, "markers must not be counted as files");
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
