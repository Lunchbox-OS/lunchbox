//! Generic least-recently-used eviction for an on-disk file cache.
//!
//! Shared by both media front-ends' video caches. Those caches otherwise
//! differ in structure — the Linux binary keys files by item id and tracks
//! completion with `.done` sentinels; the Android app keys by URL hash — so
//! only the eviction *policy* is shared here: given the cached files with their
//! sizes and a recency key, delete the oldest until the total is within a byte
//! cap. Each caller scans its own directory (applying its own filters, building
//! its own recency key) and supplies an `on_evict` hook for any paired
//! bookkeeping (deleting a sentinel, logging).
//!
//! Deliberately std-only: no networking or image work, so it stays reusable and
//! cross-compiles for Android like the rest of this crate.

use std::path::{Path, PathBuf};

/// A cached file eligible for eviction: its path, size in bytes, and a recency
/// key that orders least- to most-recently-used (the smallest is evicted
/// first). Typically the file's mtime as a `SystemTime` or `filetime::FileTime`.
pub struct LruEntry<K> {
    pub path: PathBuf,
    pub size: u64,
    pub recency: K,
}

/// Delete least-recently-used files until the total size is at or below
/// `max_bytes`. A no-op when the total is already within the cap.
///
/// `on_evict` runs after each successful removal — use it to drop a paired
/// sentinel file or log the eviction. It is not called for a file that fails to
/// delete (that file's bytes still count toward the remaining total, so the
/// loop may remove more than strictly necessary rather than spin).
pub fn evict_to_cap<K: Ord>(
    mut entries: Vec<LruEntry<K>>,
    max_bytes: u64,
    mut on_evict: impl FnMut(&Path),
) {
    let mut total: u64 = entries.iter().map(|e| e.size).sum();
    if total <= max_bytes {
        return;
    }
    // Oldest (smallest recency) first.
    entries.sort_by(|a, b| a.recency.cmp(&b.recency));
    for entry in entries {
        if total <= max_bytes {
            break;
        }
        if std::fs::remove_file(&entry.path).is_ok() {
            total = total.saturating_sub(entry.size);
            on_evict(&entry.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(dir: &Path, name: &str, size: usize) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, vec![0u8; size]).unwrap();
        p
    }

    #[test]
    fn evicts_oldest_until_within_cap() {
        // Cap 250 bytes; three 100-byte files → the oldest must be evicted.
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        let old = seed(d, "old", 100);
        let mid = seed(d, "mid", 100);
        let new = seed(d, "new", 100);
        let mut evicted = Vec::new();
        evict_to_cap(
            vec![
                LruEntry { path: old.clone(), size: 100, recency: 1u64 },
                LruEntry { path: mid.clone(), size: 100, recency: 2 },
                LruEntry { path: new.clone(), size: 100, recency: 3 },
            ],
            250,
            |p| evicted.push(p.to_path_buf()),
        );
        assert!(!old.exists(), "oldest evicted");
        assert!(mid.exists());
        assert!(new.exists());
        assert_eq!(evicted, vec![old], "on_evict fires once, for the oldest");
    }

    #[test]
    fn noop_when_within_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let a = seed(tmp.path(), "a", 100);
        let mut hook_ran = false;
        evict_to_cap(
            vec![LruEntry { path: a.clone(), size: 100, recency: 1u64 }],
            1_000,
            |_| hook_ran = true,
        );
        assert!(a.exists());
        assert!(!hook_ran);
    }
}
