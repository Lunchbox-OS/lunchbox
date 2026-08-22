//! The "somebody watched this" marker, shared by both front-ends' video caches.
//!
//! A cache that also downloads speculatively has to tell the two apart. A file
//! nobody has opened is a guess and may be replaced; a file somebody chose to
//! watch is not, and eviction should spend every guess before it touches one.
//! Without a marker, both look identical on disk — an mtime says when a file
//! arrived, not whether it mattered to anyone.
//!
//! The marker is an empty `<key>.played` file whose mtime is when playback last
//! started. It is deliberately **never removed**, including when the video it
//! names is evicted: interest in a video outlives any particular copy of it, so
//! a re-download later starts out protected rather than as a fresh guess.
//!
//! Callers choose the key. `shepherd-media-cache` uses an interest key, so
//! watching a video at one quality protects the copy cached at another; the
//! Android cache has one rendition per URL and uses its content key.
//!
//! There is a second marker here, `<key>.seen`, recording when the item was
//! *first offered* to this device rather than watched. Eviction needs to tell
//! "the parent added this yesterday" from "this has been sitting unwatched
//! since spring", and a file's own mtime cannot: it says when the download
//! landed, which resets every time the item churns through the cache. Like
//! `.played` it is keyed by interest and never removed, so an item that has
//! been evicted and re-fetched twice is still correctly old.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use filetime::FileTime;

/// Extension of the played marker. Callers that walk a cache directory must
/// skip files with this suffix — a marker is bookkeeping, not cached content.
pub const MARKER_EXT: &str = "played";

/// Extension of the first-seen marker. Skipped by directory walks for the same
/// reason as [`MARKER_EXT`].
pub const SEEN_EXT: &str = "seen";

/// Path of the played marker for `key` in `dir`.
pub fn marker_path(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("{key}.{MARKER_EXT}"))
}

/// Path of the first-seen marker for `key` in `dir`.
pub fn seen_path(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("{key}.{SEEN_EXT}"))
}

/// Whether `name` is a marker file rather than cached content.
pub fn is_marker(name: &str) -> bool {
    name.ends_with(".played") || name.ends_with(".seen")
}

/// Record that the item under `key` was played, now.
///
/// Call this when playback actually starts, not when the cache is merely
/// inspected — enumerating a cache, which a prefetcher does, must not make
/// every guess look watched.
///
/// Failures are swallowed: a cache is best-effort, and a missing marker costs
/// at most one file's eviction priority.
pub fn mark_played(dir: &Path, key: &str) -> std::io::Result<()> {
    let path = marker_path(dir, key);
    std::fs::write(&path, b"")?;
    // Rewriting an existing marker leaves its mtime at "now", which is the
    // recency watched files are ordered by.
    let _ = filetime::set_file_mtime(&path, FileTime::now());
    Ok(())
}

/// When the item under `key` was last played, or `None` if it never was.
pub fn played_at(dir: &Path, key: &str) -> Option<SystemTime> {
    marker_path(dir, key).metadata().ok()?.modified().ok()
}

/// Record that the item under `key` has been offered to this device, if that
/// has not been recorded already.
///
/// Unlike [`mark_played`] this is **write-once**: the value wanted is when the
/// item first appeared, so re-stamping it every sweep would make a year-old
/// library item look like today's addition. Callers stamp every item they walk,
/// not only the ones they download — an item that never fits in the cache still
/// has to be correctly aged when space finally frees up.
pub fn mark_seen(dir: &Path, key: &str) -> std::io::Result<()> {
    let path = seen_path(dir, key);
    if path.exists() {
        return Ok(());
    }
    std::fs::write(&path, b"")
}

/// When the item under `key` was first offered to this device, or `None` if
/// that was never recorded.
pub fn first_seen_at(dir: &Path, key: &str) -> Option<SystemTime> {
    seen_path(dir, key).metadata().ok()?.modified().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unmarked_key_has_never_been_played() {
        let dir = tempfile::tempdir().unwrap();
        assert!(played_at(dir.path(), "abc").is_none());
    }

    #[test]
    fn marking_records_a_time() {
        let dir = tempfile::tempdir().unwrap();
        mark_played(dir.path(), "abc").unwrap();
        assert!(played_at(dir.path(), "abc").is_some());
    }

    #[test]
    fn remarking_moves_the_time_forward() {
        let dir = tempfile::tempdir().unwrap();
        mark_played(dir.path(), "abc").unwrap();
        filetime::set_file_mtime(
            marker_path(dir.path(), "abc"),
            FileTime::from_unix_time(FileTime::now().unix_seconds() - 3600, 0),
        )
        .unwrap();
        let old = played_at(dir.path(), "abc").unwrap();

        mark_played(dir.path(), "abc").unwrap();
        assert!(played_at(dir.path(), "abc").unwrap() > old);
    }

    #[test]
    fn markers_are_recognisable_as_bookkeeping() {
        assert!(is_marker("abc.played"));
        assert!(is_marker("abc.seen"));
        assert!(!is_marker("abc.mp4"));
        assert!(!is_marker("abc.done"));
    }

    #[test]
    fn an_unstamped_key_has_never_been_seen() {
        let dir = tempfile::tempdir().unwrap();
        assert!(first_seen_at(dir.path(), "abc").is_none());
    }

    #[test]
    fn the_first_sighting_is_the_one_that_sticks() {
        // Re-stamping every sweep would make every library item look new, which
        // is the opposite of what the marker exists to say.
        let dir = tempfile::tempdir().unwrap();
        mark_seen(dir.path(), "abc").unwrap();
        filetime::set_file_mtime(
            seen_path(dir.path(), "abc"),
            FileTime::from_unix_time(FileTime::now().unix_seconds() - 86_400, 0),
        )
        .unwrap();
        let first = first_seen_at(dir.path(), "abc").unwrap();

        mark_seen(dir.path(), "abc").unwrap();
        assert_eq!(
            first_seen_at(dir.path(), "abc").unwrap(),
            first,
            "a second sighting must not move the timestamp"
        );
    }
}
