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

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use filetime::FileTime;

/// Extension of the marker file. Callers that walk a cache directory must skip
/// files with this suffix — a marker is bookkeeping, not cached content.
pub const MARKER_EXT: &str = "played";

/// Path of the marker for `key` in `dir`.
pub fn marker_path(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("{key}.{MARKER_EXT}"))
}

/// Whether `name` is a marker file rather than cached content.
pub fn is_marker(name: &str) -> bool {
    name.ends_with(".played")
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
        assert!(!is_marker("abc.mp4"));
        assert!(!is_marker("abc.done"));
    }
}
