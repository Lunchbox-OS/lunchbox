//! The on-disk cache layout: what a committed download looks like, how recently
//! it mattered to anyone, and which files eviction is allowed to take.
//!
//! One flat directory holds, per content key (see [`crate::key`]):
//!
//! | File | Meaning |
//! |---|---|
//! | `<key>.<ext>` | the video |
//! | `<key>.done` | commit sentinel; records the interest key and selector |
//! | `<key>.part` | an in-flight direct-HTTP download |
//! | `<key>.lock` | download claim (see [`crate::lock`]) |
//!
//! plus, per *interest* key, `<ikey>.played` — written the first time the video
//! is played from cache, and never removed. It is what separates "the child
//! watched this" from "we guessed they might".
//!
//! **Files from before URL keying are left in place.** They are named after
//! library item ids, so nothing will ever ask for them again, but they carry
//! valid sentinels — which makes them ordinary eviction candidates that age out
//! on their own. Migrating them would mean keeping a rename map forever to save
//! a download that is, by definition, re-downloadable.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use shepherd_media_app::interest;
use shepherd_media_app::lru::{self, LruEntry, Recency};
use tracing::{info, warn};

use crate::lock::is_lock_file;

/// What the cache holds for a content key.
///
/// There is no "stale" state any more: the yt-dlp selector is part of the
/// content key, so a file downloaded under a different one lives under a
/// different name and cannot be mistaken for this one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheState {
    /// Nothing committed for this key.
    Absent,
    /// A committed, playable file.
    Present,
}

/// Whether `name` is bookkeeping rather than a cached video.
fn is_sidecar(name: &str) -> bool {
    name.ends_with(".part")
        || name.ends_with(".done")
        || interest::is_marker(name)
        || is_lock_file(name)
}

/// Scan `cache_dir` for a completed download named `<key>.<ext>`.
///
/// Requires the sentinel `<key>.done`; without it the download is considered
/// in-progress (yt-dlp may have written intermediate per-format files that are
/// not yet merged) and `None` is returned.
pub fn find_cached_file(cache_dir: &Path, key: &str) -> Option<PathBuf> {
    // The sentinel is written only after the download fully commits.
    if !cache_dir.join(format!("{key}.done")).exists() {
        return None;
    }
    let prefix = format!("{key}.");
    for entry in std::fs::read_dir(cache_dir).ok()?.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with(&prefix) && !is_sidecar(&name_str) {
            return Some(entry.path());
        }
    }
    None
}

/// Write the completion sentinel for `key`, recording the interest key the
/// video's `.played` marker lives under and the selector it was fetched with.
///
/// The interest key has to be recorded rather than recomputed: walking the
/// directory yields content keys, and a hash cannot be run backwards to the URL
/// that would produce the interest key. The selector is kept for debugging
/// only — it is part of the content key now, so nothing compares it.
pub fn write_done_sentinel(
    cache_dir: &Path,
    key: &str,
    interest_key: &str,
    selector: &str,
) -> Result<(), String> {
    let path = cache_dir.join(format!("{key}.done"));
    let body = format!("interest={interest_key}\nselector={selector}\n");
    std::fs::write(&path, body.as_bytes())
        .map_err(|e| format!("failed to write done sentinel: {e}"))
}

/// The interest key recorded in `key`'s sentinel, if it has one.
///
/// Sentinels written before this field existed hold a bare selector string and
/// yield `None`; those files are the pre-hash leftovers, which are treated as
/// unwatched and age out.
fn sentinel_interest_key(cache_dir: &Path, key: &str) -> Option<String> {
    let body = std::fs::read_to_string(cache_dir.join(format!("{key}.done"))).ok()?;
    body.lines()
        .find_map(|line| line.strip_prefix("interest="))
        .map(|v| v.trim().to_string())
}

/// Classify what the cache holds for `key`.
pub fn cache_state(cache_dir: &Path, key: &str) -> CacheState {
    if find_cached_file(cache_dir, key).is_some() {
        CacheState::Present
    } else {
        CacheState::Absent
    }
}

/// Record that the video under `interest_key` was played.
///
/// Called when playback actually starts from a cached file, not when the cache
/// is merely inspected — enumerating the cache, which the prefetcher does,
/// must not make everything look watched.
///
/// The marker is never removed, including when the video is evicted: a child
/// who watched something has shown an interest in it that survives the file.
pub fn mark_played(cache_dir: &Path, interest_key: &str) {
    if let Err(e) = interest::mark_played(cache_dir, interest_key) {
        warn!("could not record playback of {interest_key}: {e}");
    }
}

/// Delete every committed file for `key` plus its sentinel.
///
/// The `.lock` file is deliberately spared. Removing it while the caller holds
/// it would leave the next claimant locking a *different* inode, and the mutual
/// exclusion it exists for would silently stop working.
pub fn remove_cached_item(cache_dir: &Path, key: &str) {
    let prefix = format!("{key}.");
    let Ok(entries) = std::fs::read_dir(cache_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with(&prefix)
            && !is_lock_file(&name_str)
            && let Err(e) = std::fs::remove_file(entry.path())
        {
            warn!("could not remove stale cache file {:?}: {e}", entry.path());
        }
    }
}

struct CacheEntry {
    path: PathBuf,
    size: u64,
    recency: Recency,
}

/// Collect all committed video files in `cache_dir` — those with a matching
/// `<key>.done` sentinel — with their sizes and recency. In-progress downloads
/// (no sentinel) are excluded so they neither count toward the size cap nor get
/// evicted mid-download. Returns `None` only if the directory cannot be read.
fn collect_cache_entries(cache_dir: &Path) -> Option<Vec<CacheEntry>> {
    let mut entries = Vec::new();
    for de in std::fs::read_dir(cache_dir).ok()?.flatten() {
        let name = de.file_name();
        let name_str = name.to_string_lossy();
        if is_sidecar(&name_str) {
            continue;
        }
        let Ok(meta) = de.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        // Derive the content key from the filename stem.
        let key = match de.path().file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };
        // Only include files whose download has been fully committed.
        if !cache_dir.join(format!("{key}.done")).exists() {
            continue;
        }

        // Watched if the video's interest marker exists; its mtime is when
        // playback last started. Otherwise the file's own mtime is when the
        // download completed.
        let played_at = sentinel_interest_key(cache_dir, &key)
            .and_then(|ikey| interest::played_at(cache_dir, &ikey));
        let recency =
            Recency::classify(meta.modified().unwrap_or(SystemTime::UNIX_EPOCH), played_at);

        entries.push(CacheEntry {
            path: de.path(),
            size: meta.len(),
            recency,
        });
    }
    Some(entries)
}

/// Total size of the committed cache, in bytes.
pub fn cache_total(cache_dir: &Path) -> u64 {
    collect_cache_entries(cache_dir)
        .map(|e| e.iter().map(|x| x.size).sum())
        .unwrap_or(0)
}

fn lru_entries(cache_dir: &Path) -> Option<Vec<LruEntry<Recency>>> {
    Some(
        collect_cache_entries(cache_dir)?
            .into_iter()
            .map(|e| LruEntry {
                path: e.path,
                size: e.size,
                recency: e.recency,
            })
            .collect(),
    )
}

fn drop_sentinel(cache_dir: &Path, path: &Path) {
    info!("evicted cached video: {}", path.display());
    if let Some(key) = path.file_stem().and_then(|s| s.to_str()) {
        let _ = std::fs::remove_file(cache_dir.join(format!("{key}.done")));
    }
}

/// Evict until the total is at or below `target_bytes`, taking anything.
///
/// Used after a download the user earned by watching the previous video: they
/// asked for this content, so it may cost them the least-recently-watched file.
pub fn evict_to(cache_dir: &Path, target_bytes: u64) {
    let Some(entries) = lru_entries(cache_dir) else {
        return;
    };
    lru::evict_to_cap(entries, target_bytes, |path| drop_sentinel(cache_dir, path));
}

/// Evict until the total is at or below `target_bytes`, taking **only files
/// nobody has watched**.
///
/// This is the ceiling on what a speculative prefetch may cost: it can recycle
/// space held by other guesses, and it stops rather than touch something a
/// child chose. If everything cached has been watched, the total simply stays
/// above the target and the caller skips its download.
pub fn evict_unwatched_to(cache_dir: &Path, target_bytes: u64) {
    let Some(entries) = lru_entries(cache_dir) else {
        return;
    };
    lru::evict_to_cap_where(
        entries,
        target_bytes,
        |e| e.recency.is_unwatched(),
        |path| drop_sentinel(cache_dir, path),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use filetime::FileTime;

    const H264: &str = "bv*[vcodec^=avc1][height<=?1080]+ba/b";

    /// Commit `key` to the cache as if downloaded, with `ikey` as its video's
    /// interest key.
    fn commit(dir: &Path, key: &str, ikey: &str, ext: &str) {
        std::fs::write(dir.join(format!("{key}.{ext}")), b"video").unwrap();
        write_done_sentinel(dir, key, ikey, H264).unwrap();
    }

    fn age(dir: &Path, name: &str, secs_ago: i64) {
        filetime::set_file_mtime(
            dir.join(name),
            FileTime::from_unix_time(FileTime::now().unix_seconds() - secs_ago, 0),
        )
        .unwrap();
    }

    #[test]
    fn absent_when_nothing_committed() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(cache_state(dir.path(), "clip"), CacheState::Absent);
    }

    #[test]
    fn absent_while_a_download_is_still_in_flight() {
        // A file with no sentinel is an unfinished download, not a broken one —
        // treating it as present would hand a partial file to the player.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("clip.part"), b"partial").unwrap();
        assert_eq!(cache_state(dir.path(), "clip"), CacheState::Absent);
    }

    #[test]
    fn present_once_committed() {
        let dir = tempfile::tempdir().unwrap();
        commit(dir.path(), "clip", "ikey", "mp4");
        assert_eq!(cache_state(dir.path(), "clip"), CacheState::Present);
    }

    #[test]
    fn removing_an_item_clears_every_extension_and_the_sentinel() {
        let dir = tempfile::tempdir().unwrap();
        commit(dir.path(), "clip", "ikey", "webm");
        std::fs::write(dir.path().join("clip.part"), b"partial").unwrap();
        // A different item must survive.
        commit(dir.path(), "other", "ikey2", "mp4");

        remove_cached_item(dir.path(), "clip");

        assert_eq!(cache_state(dir.path(), "clip"), CacheState::Absent);
        assert!(!dir.path().join("clip.part").exists());
        assert_eq!(cache_state(dir.path(), "other"), CacheState::Present);
    }

    #[test]
    fn a_lock_file_is_not_content() {
        // It must not be served as the video, counted toward the cap, or
        // deleted along with the item it guards.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("clip.lock"), b"").unwrap();
        commit(dir.path(), "clip", "ikey", "mp4");

        assert_eq!(
            find_cached_file(dir.path(), "clip").unwrap().extension(),
            Some("mp4".as_ref())
        );
        assert_eq!(cache_total(dir.path()), b"video".len() as u64);

        remove_cached_item(dir.path(), "clip");
        assert!(
            dir.path().join("clip.lock").exists(),
            "the lock must survive so the holder keeps its inode"
        );
    }

    #[test]
    fn a_played_marker_is_not_content() {
        let dir = tempfile::tempdir().unwrap();
        commit(dir.path(), "clip", "ikey", "mp4");
        mark_played(dir.path(), "ikey");
        assert_eq!(
            cache_total(dir.path()),
            b"video".len() as u64,
            "the marker must not count toward the cap"
        );
        assert_eq!(
            find_cached_file(dir.path(), "clip").unwrap().extension(),
            Some("mp4".as_ref())
        );
    }

    // --- eviction ordering (issue #127 phase 3) ---

    #[test]
    fn watched_files_outrank_unwatched_ones_however_old() {
        // The headline: a film watched a month ago beats a prefetch that
        // arrived a minute ago. Under plain mtime ordering it lost.
        let dir = tempfile::tempdir().unwrap();
        commit(dir.path(), "watched", "iw", "mp4");
        commit(dir.path(), "guessed", "ig", "mp4");
        mark_played(dir.path(), "iw");
        age(dir.path(), "iw.played", 30 * 24 * 3600);

        evict_to(dir.path(), b"video".len() as u64);

        assert_eq!(cache_state(dir.path(), "watched"), CacheState::Present);
        assert_eq!(cache_state(dir.path(), "guessed"), CacheState::Absent);
    }

    #[test]
    fn among_unwatched_the_newest_download_goes_first() {
        // Prefetch walks a library in display order, so the newest arrival is
        // the furthest down the list. Evicting the head instead would have the
        // next pass re-download it immediately.
        let dir = tempfile::tempdir().unwrap();
        commit(dir.path(), "head", "ih", "mp4");
        commit(dir.path(), "tail", "it", "mp4");
        age(dir.path(), "head.mp4", 3600);

        evict_to(dir.path(), b"video".len() as u64);

        assert_eq!(cache_state(dir.path(), "head"), CacheState::Present);
        assert_eq!(cache_state(dir.path(), "tail"), CacheState::Absent);
    }

    #[test]
    fn among_watched_the_least_recently_played_goes_first() {
        let dir = tempfile::tempdir().unwrap();
        commit(dir.path(), "old", "io", "mp4");
        commit(dir.path(), "new", "in", "mp4");
        mark_played(dir.path(), "io");
        mark_played(dir.path(), "in");
        age(dir.path(), "io.played", 3600);

        evict_to(dir.path(), b"video".len() as u64);

        assert_eq!(cache_state(dir.path(), "old"), CacheState::Absent);
        assert_eq!(cache_state(dir.path(), "new"), CacheState::Present);
    }

    #[test]
    fn a_prefetch_may_recycle_unwatched_space() {
        // What stops the cache going inert: guessed downloads are replaceable.
        let dir = tempfile::tempdir().unwrap();
        commit(dir.path(), "guessed", "ig", "mp4");
        evict_unwatched_to(dir.path(), 0);
        assert_eq!(cache_state(dir.path(), "guessed"), CacheState::Absent);
    }

    #[test]
    fn a_prefetch_will_not_displace_something_watched() {
        let dir = tempfile::tempdir().unwrap();
        commit(dir.path(), "watched", "iw", "mp4");
        mark_played(dir.path(), "iw");

        evict_unwatched_to(dir.path(), 0);

        assert_eq!(
            cache_state(dir.path(), "watched"),
            CacheState::Present,
            "a guess must never cost the child something they chose"
        );
    }

    #[test]
    fn a_played_marker_outlives_the_video_it_names() {
        // Interest in a video survives the file: re-downloaded later, it is
        // still content the child has chosen once.
        let dir = tempfile::tempdir().unwrap();
        commit(dir.path(), "clip", "ikey", "mp4");
        mark_played(dir.path(), "ikey");

        evict_to(dir.path(), 0);

        assert_eq!(cache_state(dir.path(), "clip"), CacheState::Absent);
        assert!(dir.path().join("ikey.played").exists());
    }

    /// Files predating the current keying are ordinary eviction candidates,
    /// not a special case: they carry sentinels, count toward the cap, and —
    /// having no recorded interest key — sort as unwatched.
    #[test]
    fn pre_hash_files_still_count_and_still_evict() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("big-buck-bunny.mp4"), b"video").unwrap();
        // The phase-2 sentinel format: a bare selector, no interest key.
        std::fs::write(dir.path().join("big-buck-bunny.done"), H264.as_bytes()).unwrap();

        assert_eq!(cache_total(dir.path()), b"video".len() as u64);
        evict_unwatched_to(dir.path(), 0);
        assert!(find_cached_file(dir.path(), "big-buck-bunny").is_none());
    }
}
