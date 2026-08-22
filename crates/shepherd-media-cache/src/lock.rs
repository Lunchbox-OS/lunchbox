//! Per-key download locks.
//!
//! Two processes now share this cache: a running `shepherd-media` and (from
//! issue #127 phase 3) shepherdd's background prefetcher. Both can classify the
//! same key as [`CacheState::Absent`](crate::store::CacheState) at the same
//! moment and start downloading into the same `.part` file, producing a
//! corrupt result and two videos' worth of bandwidth.
//!
//! The `.done` sentinel makes *readers* safe — it is written only after a
//! download commits — but arbitrates nothing between writers. This does: an
//! `flock` on `<key>.lock`, taken non-blocking, held for the download.
//!
//! **The loser skips; it never waits.** A prefetch is best-effort, and the play
//! path streams the remote source rather than stall a child behind a progress
//! bar they cannot see. Both callers therefore lose nothing by giving up.
//!
//! Lock files are left behind on purpose: deleting one while another process
//! holds it open would hand the next caller a lock on a fresh inode, and mutual
//! exclusion would quietly stop working. They are empty, so the cost is one
//! directory entry per distinct URL ever downloaded.
//!
//! `flock` rather than an "is the lock file there?" check for one reason that
//! matters here: the kernel releases it when the holder's fd closes, including
//! when the process is killed. Activities get SIGTERM'd mid-download every time
//! a session ends, and a presence-based lock would strand that item as
//! permanently un-downloadable.

use std::fs::{File, OpenOptions};
use std::path::Path;

use nix::fcntl::{Flock, FlockArg};
use tracing::debug;

/// A held exclusive download lock. Releases on drop.
pub struct DownloadLock {
    // Kept for its `Drop`, which releases the flock.
    _flock: Flock<File>,
}

impl DownloadLock {
    /// Try to claim `key` for downloading.
    ///
    /// Returns `None` when another process holds it (its download is in
    /// flight), or when the lock file cannot be created at all — in both cases
    /// the caller should skip rather than download unprotected.
    pub fn try_acquire(cache_dir: &Path, key: &str) -> Option<Self> {
        let path = cache_dir.join(format!("{key}.lock"));
        let file = match OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
        {
            Ok(f) => f,
            Err(e) => {
                debug!("could not open download lock {}: {e}", path.display());
                return None;
            }
        };

        match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
            Ok(flock) => Some(Self { _flock: flock }),
            Err(_) => {
                debug!("download already in flight elsewhere for {key}");
                None
            }
        }
    }
}

/// Whether `name` is a lock file rather than cached content. The store skips
/// these everywhere it walks the directory: they are not videos, must not count
/// toward the size cap, and must not be deleted while held.
pub fn is_lock_file(name: &str) -> bool {
    name.ends_with(".lock")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_claim_on_the_same_key_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let first = DownloadLock::try_acquire(dir.path(), "abc").expect("first claim succeeds");
        // Same process, same key: `flock` is per open file description, so a
        // second `open` + `flock` contends exactly as another process would.
        assert!(
            DownloadLock::try_acquire(dir.path(), "abc").is_none(),
            "a second claim must be refused while the first is held"
        );
        drop(first);
        assert!(
            DownloadLock::try_acquire(dir.path(), "abc").is_some(),
            "the lock must be released on drop"
        );
    }

    #[test]
    fn different_keys_do_not_contend() {
        let dir = tempfile::tempdir().unwrap();
        let _a = DownloadLock::try_acquire(dir.path(), "aaa").unwrap();
        assert!(
            DownloadLock::try_acquire(dir.path(), "bbb").is_some(),
            "two items must be able to download concurrently"
        );
    }
}
