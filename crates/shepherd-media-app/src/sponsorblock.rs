//! The on-disk half of the SponsorBlock lookup, shared by both front-ends
//! (issue #159).
//!
//! Same split as [`RemotePosterCache`](crate::poster_cache): the disk cache and
//! its policy live here, and each platform supplies the HTTP fetch and the
//! directory to keep files in. The Linux binary and shepherdd cache under
//! `$XDG_CACHE_HOME`; the Android app uses app-private storage; both then spend
//! the same bytes by the same rules.
//!
//! ## Files are buckets, and the bytes are the server's
//!
//! One file per hash prefix, named `<prefix>.json`, holding the response
//! verbatim. Nothing is re-encoded: `shepherd_media_core::sponsorblock` owns the
//! wire format and is the only thing that parses it, so a schema invented here
//! would be a second one to keep in step for no gain. The file's mtime is the
//! fetch time, which is one fewer field that can disagree with the file it is
//! stored in.
//!
//! Caching per *bucket* rather than per video is what makes the private
//! endpoint affordable: one request covers around a hundred videos, so a lookup
//! that lands in a bucket already on disk costs nothing at all.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::cache::{self, Freshness};
use crate::cache_key::sponsorblock_prefix;

/// How long a cached bucket is trusted. Submissions churn most in the days
/// after an upload, and a day-old bucket that skips one segment fewer is a much
/// smaller cost than a network round-trip in front of every video.
pub const DEFAULT_TTL: Duration = Duration::from_secs(24 * 3600);

/// A directory of cached SponsorBlock buckets.
pub struct BucketStore {
    /// `None` when the platform could determine no cache directory, in which
    /// case every lookup is a live fetch and nothing is written down.
    dir: Option<PathBuf>,
    ttl: Duration,
}

impl BucketStore {
    pub fn new(dir: Option<PathBuf>, ttl: Duration) -> Self {
        Self { dir, ttl }
    }

    /// The bucket covering `video_id`, fetching it when the cache cannot serve
    /// it.
    ///
    /// `fetch` is given the hash prefix — never the video id, which is the whole
    /// point of the endpoint — and returns the raw response or an error message.
    /// It is called only on a miss or past the TTL.
    ///
    /// `None` means there is nothing to work with: no cached bucket and no
    /// successful fetch. That is not an error anybody needs told about; it means
    /// the video plays through.
    pub fn resolve(
        &self,
        video_id: &str,
        fetch: impl FnOnce(&str) -> Result<String, String>,
    ) -> Option<String> {
        let prefix = sponsorblock_prefix(video_id);
        let path = self.path_for(&prefix);
        let cached = path.as_deref().and_then(|p| self.load(p));

        match cache::resolve(cached, || fetch(&prefix)) {
            cache::Resolution::Fresh(json) => Some(json),
            cache::Resolution::Fetched(json) => {
                if let Some(path) = path.as_deref() {
                    save(path, &json);
                }
                Some(json)
            }
            // Offline past the TTL: yesterday's segments are much better than
            // none, and this is the case the cache exists for.
            cache::Resolution::Stale(json, e) => {
                log::debug!("SponsorBlock refresh for {prefix} failed: {e}; using stale bucket");
                Some(json)
            }
            cache::Resolution::Miss(e) => {
                log::debug!("no SponsorBlock segments for {prefix}: {e}");
                None
            }
        }
    }

    /// Re-fetch the bucket covering `video_id` whatever its age, for an
    /// administrator-triggered refresh (issue #165).
    ///
    /// The TTL is the only thing skipped. On a failed fetch the cached bucket
    /// is deliberately **left where it is**: every other cache on this path
    /// falls back to a stale copy when the network is gone, and a refresh that
    /// deleted what it could not replace would leave the device skipping
    /// nothing — worse than before the button was pressed. The error is
    /// returned instead, so the caller can say the refresh did not happen.
    ///
    /// `Ok` means the bucket on disk is now the service's current answer.
    pub fn refresh(
        &self,
        video_id: &str,
        fetch: impl FnOnce(&str) -> Result<String, String>,
    ) -> Result<(), String> {
        let prefix = sponsorblock_prefix(video_id);
        let json = fetch(&prefix)?;
        if let Some(path) = self.path_for(&prefix) {
            save(&path, &json);
        }
        Ok(())
    }

    /// The hash prefix `video_id` falls in. Exposed so a caller refreshing a
    /// whole library can fetch each bucket once rather than once per video —
    /// around a hundred videos share one.
    pub fn prefix_for(&self, video_id: &str) -> String {
        sponsorblock_prefix(video_id)
    }

    /// Where `prefix` is cached, or `None` when there is no cache directory.
    pub fn path_for(&self, prefix: &str) -> Option<PathBuf> {
        self.dir.as_ref().map(|d| d.join(format!("{prefix}.json")))
    }

    /// Read a cached bucket and judge its age by the file's mtime.
    ///
    /// An unreadable file is a miss; the caller re-fetches, which is always
    /// safe. There is deliberately no parse here — the core does that, and a
    /// torn or truncated file should present as "no segments" rather than as a
    /// cache that has to know the format.
    fn load(&self, path: &Path) -> Option<(String, Freshness)> {
        let json = std::fs::read_to_string(path).ok()?;
        let age = std::fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .unwrap_or(Duration::ZERO);

        let freshness = if age >= self.ttl {
            Freshness::Stale
        } else {
            Freshness::Fresh
        };
        Some((json, freshness))
    }
}

/// Write a bucket, atomically.
///
/// Through a temporary file and a rename because two processes share this
/// directory on Linux — shepherdd's prefetcher warms buckets while a player
/// reads them — and a torn read would present as "no segments" rather than as
/// an error anybody could see.
///
/// Failures are logged and swallowed: a cache that could not be written costs a
/// fetch next time and nothing else.
fn save(path: &Path, json: &str) {
    let Some(parent) = path.parent() else { return };
    if let Err(e) = std::fs::create_dir_all(parent) {
        log::warn!(
            "could not create the SponsorBlock cache dir {}: {e}",
            parent.display()
        );
        return;
    }

    let tmp = path.with_extension("json.tmp");
    if let Err(e) = std::fs::write(&tmp, json) {
        log::warn!("could not write {}: {e}", tmp.display());
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        log::warn!("could not commit {}: {e}", path.display());
        let _ = std::fs::remove_file(&tmp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    const VIDEO: &str = "dQw4w9WgXcQ";
    const PREFIX: &str = "5f6b";

    fn store_in(dir: &Path) -> BucketStore {
        BucketStore::new(Some(dir.to_path_buf()), DEFAULT_TTL)
    }

    /// Backdate a cached bucket so it reads as stale.
    fn age(path: &Path, seconds: u64) {
        let when = SystemTime::now() - Duration::from_secs(seconds);
        let ft = filetime::FileTime::from_unix_time(
            when.duration_since(UNIX_EPOCH).unwrap().as_secs() as i64,
            0,
        );
        filetime::set_file_mtime(path, ft).unwrap();
    }

    #[test]
    fn a_fetch_is_asked_for_the_prefix_and_written_under_it() {
        let dir = tempfile::tempdir().unwrap();
        let json = store_in(dir.path())
            .resolve(VIDEO, |prefix| {
                assert_eq!(prefix, PREFIX, "the video id must never be sent");
                Ok("[]".to_string())
            })
            .unwrap();
        assert_eq!(json, "[]");

        let path = dir.path().join(format!("{PREFIX}.json"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[]");
        assert!(
            std::fs::read_dir(dir.path()).unwrap().all(|e| !e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")),
            "the temporary file should not survive the rename"
        );
    }

    #[test]
    fn a_fresh_bucket_is_served_without_a_fetch() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        store.resolve(VIDEO, |_| Ok("first".into())).unwrap();
        assert_eq!(
            store
                .resolve(VIDEO, |_| panic!("a fresh bucket must not be refetched"))
                .unwrap(),
            "first"
        );
    }

    #[test]
    fn a_stale_bucket_is_refreshed_when_the_fetch_works() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        store.resolve(VIDEO, |_| Ok("old".into())).unwrap();
        age(&dir.path().join(format!("{PREFIX}.json")), 48 * 3600);

        assert_eq!(store.resolve(VIDEO, |_| Ok("new".into())).unwrap(), "new");
        assert_eq!(
            std::fs::read_to_string(dir.path().join(format!("{PREFIX}.json"))).unwrap(),
            "new"
        );
    }

    #[test]
    fn a_refresh_refetches_a_bucket_that_is_still_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        store.resolve(VIDEO, |_| Ok("old".into())).unwrap();

        store
            .refresh(VIDEO, |prefix| {
                assert_eq!(prefix, PREFIX, "the video id must never be sent");
                Ok("new".to_string())
            })
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join(format!("{PREFIX}.json"))).unwrap(),
            "new"
        );
    }

    /// The reason a refresh re-fetches rather than deleting-then-fetching: a
    /// button pressed on a flaky network must not leave the device with fewer
    /// segments than it had.
    #[test]
    fn a_failed_refresh_keeps_the_bucket_it_could_not_replace() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        store.resolve(VIDEO, |_| Ok("old".into())).unwrap();

        assert!(store.refresh(VIDEO, |_| Err("offline".into())).is_err());
        assert_eq!(
            store
                .resolve(VIDEO, |_| panic!("the kept bucket is still fresh"))
                .unwrap(),
            "old"
        );
    }

    /// The offline case the whole cache exists for: a device past the TTL with
    /// no network still skips what it knew about yesterday.
    #[test]
    fn a_stale_bucket_is_served_when_the_fetch_fails() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        store.resolve(VIDEO, |_| Ok("old".into())).unwrap();
        age(&dir.path().join(format!("{PREFIX}.json")), 48 * 3600);

        assert_eq!(
            store.resolve(VIDEO, |_| Err("offline".into())).unwrap(),
            "old"
        );
    }

    #[test]
    fn a_miss_with_no_network_yields_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            store_in(dir.path())
                .resolve(VIDEO, |_| Err("offline".into()))
                .is_none()
        );
    }

    /// Two videos in one bucket share a file, which is what makes prefetching a
    /// playlist affordable.
    #[test]
    fn a_second_video_in_the_same_bucket_needs_no_fetch() {
        let dir = tempfile::tempdir().unwrap();
        let store = store_in(dir.path());
        store.resolve(VIDEO, |_| Ok("bucket".into())).unwrap();
        // `eXjGWlJOhWg` hashes into 5f6b as well.
        assert_eq!(
            store
                .resolve("eXjGWlJOhWg", |_| panic!("already on disk"))
                .unwrap(),
            "bucket"
        );
    }

    #[test]
    fn with_no_directory_nothing_is_written_and_every_lookup_fetches() {
        let store = BucketStore::new(None, DEFAULT_TTL);
        assert_eq!(store.resolve(VIDEO, |_| Ok("x".into())).unwrap(), "x");
        assert!(store.resolve(VIDEO, |_| Err("offline".into())).is_none());
        assert!(store.path_for(PREFIX).is_none());
    }
}
