//! Shared on-disk cache for remote poster images.
//!
//! Both media front-ends fetch remote posters (YouTube thumbnails, `http(s)`
//! poster URLs) and cache the encoded bytes on disk so they survive across
//! launches and render while offline. This module owns the platform-agnostic
//! half of that: the URL→file mapping, the freshness (TTL) check, the
//! write-back, and the offline stale-fallback policy.
//!
//! It is deliberately free of networking and image decoding — the caller
//! injects the fetch (each platform bounds and performs the HTTP request its
//! own way) and decodes the returned bytes itself. That keeps this reusable by
//! the Linux binary (lazy egui image loader) and the Android app (worker-thread
//! decode to `ColorImage`, capped download) alike.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::cache::{self, Freshness};

/// Default freshness window for a cached poster. Matches the playlist metadata
/// cache so the two evict — and fall back offline — on the same cadence.
pub const DEFAULT_TTL: Duration = Duration::from_secs(6 * 3600);

/// Outcome of resolving a poster URL against the cache and, when needed, a live
/// fetch — the shared [`cache::Resolution`] specialized to poster bytes.
pub type Resolution = cache::Resolution<Vec<u8>>;

/// A URL-keyed on-disk cache of remote poster bytes.
///
/// Keyed by a `DefaultHasher` of the URL (not stable across Rust versions —
/// acceptable, since a toolchain bump just re-fetches). Cheap to clone (a dir
/// path + a `Duration`) so each worker thread can hold its own handle.
#[derive(Clone)]
pub struct RemotePosterCache {
    dir: PathBuf,
    ttl: Duration,
}

impl RemotePosterCache {
    /// Create a cache rooted at `dir` (created lazily on first write) with the
    /// given freshness window. Use [`DEFAULT_TTL`] to match the other caches.
    pub fn new(dir: PathBuf, ttl: Duration) -> Self {
        Self { dir, ttl }
    }

    /// The on-disk path a URL's bytes are cached at.
    pub fn path_for(&self, url: &str) -> PathBuf {
        let mut h = DefaultHasher::new();
        url.hash(&mut h);
        self.dir.join(format!("{:016x}.bin", h.finish()))
    }

    /// Resolve `url` to poster bytes: serve a fresh cache entry directly;
    /// otherwise attempt `fetch` and, on failure, fall back to any stale entry.
    /// A successful fetch is written back to disk before returning.
    ///
    /// `fetch` returns `Ok(bytes)` or `Err(message)`; it is only called on a
    /// cache miss or a stale entry.
    pub fn resolve(
        &self,
        url: &str,
        fetch: impl FnOnce() -> Result<Vec<u8>, String>,
    ) -> Resolution {
        let path = self.path_for(url);
        let cached = read_with_age(&path).map(|(bytes, age)| {
            let freshness = if age < self.ttl {
                Freshness::Fresh
            } else {
                Freshness::Stale
            };
            (bytes, freshness)
        });

        let resolution = cache::resolve(cached, fetch);
        // Persist a freshly fetched poster so the next launch is a cache hit.
        if let cache::Resolution::Fetched(bytes) = &resolution {
            self.write(&path, bytes);
        }
        resolution
    }

    /// Convenience over [`resolve`](Self::resolve) for callers that don't need
    /// per-outcome logging: returns the bytes for a fresh/fetched/stale result
    /// and `None` on a total miss. `fetch` returns `Some(bytes)` on success.
    pub fn load(&self, url: &str, fetch: impl FnOnce() -> Option<Vec<u8>>) -> Option<Vec<u8>> {
        match self.resolve(url, || fetch().ok_or_else(String::new)) {
            Resolution::Fresh(b) | Resolution::Fetched(b) | Resolution::Stale(b, _) => Some(b),
            Resolution::Miss(_) => None,
        }
    }

    fn write(&self, path: &Path, bytes: &[u8]) {
        let _ = std::fs::create_dir_all(&self.dir);
        let _ = std::fs::write(path, bytes);
    }
}

/// Read a cached file with its age, or `None` if it is absent or unreadable.
fn read_with_age(path: &Path) -> Option<(Vec<u8>, Duration)> {
    let bytes = std::fs::read(path).ok()?;
    let age = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .unwrap_or(Duration::ZERO);
    Some((bytes, age))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache(name: &str, ttl: Duration) -> (tempfile::TempDir, RemotePosterCache) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join(name);
        (tmp, RemotePosterCache::new(dir, ttl))
    }

    const URL: &str = "https://unreachable.invalid/poster.png";

    #[test]
    fn fresh_cache_hit_skips_fetch() {
        let (_tmp, c) = cache("fresh", DEFAULT_TTL);
        c.write(&c.path_for(URL), &[1, 2, 3]);
        let res = c.resolve(URL, || panic!("must not fetch on a fresh cache hit"));
        assert!(matches!(res, Resolution::Fresh(b) if b == vec![1, 2, 3]));
    }

    #[test]
    fn miss_fetches_and_writes_back() {
        let (_tmp, c) = cache("miss", DEFAULT_TTL);
        let res = c.resolve(URL, || Ok(vec![9, 9]));
        assert!(matches!(res, Resolution::Fetched(b) if b == vec![9, 9]));
        // Written back: a subsequent resolve is a fresh hit without fetching.
        let again = c.resolve(URL, || panic!("should be cached now"));
        assert!(matches!(again, Resolution::Fresh(b) if b == vec![9, 9]));
    }

    #[test]
    fn stale_prefers_a_fresh_fetch() {
        // ttl = 0 makes any existing entry stale (age >= 0 is never < 0).
        let (_tmp, c) = cache("stale-fetch", Duration::ZERO);
        c.write(&c.path_for(URL), &[1]);
        let res = c.resolve(URL, || Ok(vec![2]));
        assert!(matches!(res, Resolution::Fetched(b) if b == vec![2]));
    }

    #[test]
    fn stale_falls_back_when_fetch_fails_offline() {
        // The #64 regression, now covered once for both platforms: an expired
        // poster cache must still serve its bytes when the network is down.
        let (_tmp, c) = cache("stale-offline", Duration::ZERO);
        c.write(&c.path_for(URL), &[7, 7]);
        let res = c.resolve(URL, || Err("offline".to_string()));
        assert!(matches!(res, Resolution::Stale(b, _) if b == vec![7, 7]));
    }

    #[test]
    fn miss_with_failed_fetch_yields_nothing() {
        let (_tmp, c) = cache("miss-offline", DEFAULT_TTL);
        let res = c.resolve(URL, || Err("offline".to_string()));
        assert!(matches!(res, Resolution::Miss(_)));
        // And the convenience wrapper reports None.
        assert!(c.load(URL, || None).is_none());
    }
}
