//! Poster prefetch for the browse UI.
//!
//! Posters are loaded once at startup. Local posters are read from disk;
//! remote posters are served from an on-disk cache at
//! `$XDG_CACHE_HOME/shepherd/media/posters/<hash>.bin` and only fetched over
//! HTTP on cache miss or when the cached copy has expired. A failure produces
//! a placeholder rather than aborting the launch; failed fetches are not
//! cached so a transient network blip on first launch doesn't become a
//! permanent placeholder.
//!
//! Like the playlist metadata cache (see `youtube.rs`), a *stale* cache entry
//! is used as an offline fallback: when the cached bytes have aged past
//! [`CACHE_TTL_SECS`] a fresh fetch is attempted, but if that fetch fails
//! (typically because the network is unreachable) the stale bytes are served
//! anyway. Otherwise a device that went offline more than [`CACHE_TTL_SECS`]
//! ago would lose every YouTube thumbnail it had already cached.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::time::Duration;

use shepherd_media_core::{Library, PosterRef};
use tracing::{debug, warn};

/// Cached poster bytes are considered fresh for this many seconds. Matches the
/// playlist metadata cache TTL so the two evict on roughly the same cadence.
const CACHE_TTL_SECS: u64 = 6 * 3600;

/// Raw bytes of a poster image (`PNG`/`JPEG`/`WebP`).
pub type PosterBytes = Vec<u8>;

/// Map from item id to loaded poster bytes. Items without a poster, or whose
/// poster failed to load, are absent from the map.
#[derive(Default)]
pub struct PosterCache {
    map: HashMap<String, PosterBytes>,
}

impl PosterCache {
    pub fn get(&self, item_id: &str) -> Option<&PosterBytes> {
        self.map.get(item_id)
    }

    pub fn insert(&mut self, item_id: String, bytes: PosterBytes) {
        self.map.insert(item_id, bytes);
    }
}

/// Synchronously prefetch every declared poster.
///
/// Networking happens here, in the binary, so that `shepherd-media-core`
/// stays free of network dependencies and remains buildable for Android.
pub fn prefetch(library: &Library) -> PosterCache {
    let mut cache = PosterCache::default();
    let timeout = Duration::from_secs(5);
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(timeout)
        .timeout_read(timeout)
        .build();

    for item in &library.items {
        let Some(poster) = &item.poster else { continue };
        match poster {
            PosterRef::Local(path) => match std::fs::read(path) {
                Ok(bytes) => {
                    cache.insert(item.id.clone(), bytes);
                }
                Err(e) => {
                    warn!(
                        item = item.id,
                        path = %path.display(),
                        "failed to read local poster: {e}"
                    );
                }
            },
            PosterRef::Remote(url) => {
                let url_str = url.as_str();
                let cached = load_from_disk_cache(url_str);
                match resolve_remote(cached, || fetch_remote(&agent, url_str)) {
                    Resolution::CacheHit(bytes) => {
                        debug!(item = item.id, url = %url, "poster disk cache hit");
                        cache.insert(item.id.clone(), bytes);
                    }
                    Resolution::Fetched(buf) => {
                        debug!(item = item.id, bytes = buf.len(), "fetched remote poster");
                        save_to_disk_cache(url_str, &buf);
                        cache.insert(item.id.clone(), buf);
                    }
                    Resolution::StaleFallback(bytes, e) => {
                        warn!(item = item.id, url = %url, "remote poster fetch failed: {e}; using stale cache");
                        cache.insert(item.id.clone(), bytes);
                    }
                    Resolution::Failed(e) => {
                        warn!(item = item.id, url = %url, "remote poster fetch failed: {e}");
                    }
                }
            }
        }
    }

    cache
}

/// Freshness of an on-disk poster cache hit. Mirrors the playlist cache so
/// the two evict — and fall back offline — on the same logic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CacheFreshness {
    /// Cached bytes are within [`CACHE_TTL_SECS`] of now.
    Fresh,
    /// Cached bytes are older than [`CACHE_TTL_SECS`]; usable as an offline
    /// fallback when a live fetch fails.
    Stale,
}

/// Outcome of resolving a remote poster against the disk cache and, when
/// needed, a live fetch. Returned by the pure [`resolve_remote`] so the I/O
/// (logging, disk write-back) stays at the call site.
enum Resolution {
    /// Fresh disk-cache hit; use these bytes as-is.
    CacheHit(PosterBytes),
    /// Live fetch succeeded; use these bytes and write them back to disk.
    Fetched(PosterBytes),
    /// Live fetch failed but a stale entry exists; use the stale bytes. Carries
    /// the fetch error for logging.
    StaleFallback(PosterBytes, String),
    /// No poster available; carries the fetch error for logging.
    Failed(String),
}

/// Decide which bytes to use for a remote poster.
///
/// A fresh cache entry is used directly. Otherwise — a cache miss or a stale
/// entry — a live fetch is attempted; on failure any stale bytes are used as
/// an offline fallback. This mirrors the playlist metadata cache so a device
/// that has been offline longer than the TTL keeps its cached thumbnails.
fn resolve_remote(
    cached: Option<(PosterBytes, CacheFreshness)>,
    fetch: impl FnOnce() -> Result<PosterBytes, String>,
) -> Resolution {
    if let Some((bytes, CacheFreshness::Fresh)) = cached {
        return Resolution::CacheHit(bytes);
    }
    let stale = cached.map(|(bytes, _)| bytes);
    match fetch() {
        Ok(buf) => Resolution::Fetched(buf),
        Err(e) => match stale {
            Some(bytes) => Resolution::StaleFallback(bytes, e),
            None => Resolution::Failed(e),
        },
    }
}

/// Fetch poster bytes over HTTP, returning an error string on any failure.
fn fetch_remote(agent: &ureq::Agent, url: &str) -> Result<PosterBytes, String> {
    let resp = agent.get(url).call().map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    std::io::copy(&mut resp.into_reader(), &mut buf).map_err(|e| e.to_string())?;
    Ok(buf)
}

/// Hash a URL to a 16-char hex string used as the on-disk cache filename.
///
/// `DefaultHasher` is not stable across Rust versions, which means a toolchain
/// upgrade invalidates the cache — that is acceptable: stale entries are
/// simply re-fetched on the next launch.
fn cache_filename(url: &str) -> String {
    let mut hasher = DefaultHasher::new();
    url.hash(&mut hasher);
    format!("{:016x}.bin", hasher.finish())
}

fn cache_path(url: &str) -> Option<PathBuf> {
    let cache_home = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(
        cache_home
            .join("shepherd")
            .join("media")
            .join("posters")
            .join(cache_filename(url)),
    )
}

/// Load poster bytes from the on-disk cache, reporting whether they are fresh
/// or stale. Returns `None` only on a true miss (absent or unreadable file);
/// a stale-but-readable entry is returned so the caller can use it as an
/// offline fallback after a failed live fetch.
fn load_from_disk_cache(url: &str) -> Option<(PosterBytes, CacheFreshness)> {
    let path = cache_path(url)?;
    let bytes = std::fs::read(&path).ok()?;
    let freshness = match std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|m| m.elapsed().ok())
    {
        Some(age) if age < Duration::from_secs(CACHE_TTL_SECS) => CacheFreshness::Fresh,
        _ => {
            debug!("poster cache stale for {url}");
            CacheFreshness::Stale
        }
    };
    Some((bytes, freshness))
}

fn save_to_disk_cache(url: &str, bytes: &[u8]) {
    let Some(path) = cache_path(url) else {
        return;
    };
    let Some(parent) = path.parent() else {
        return;
    };
    if let Err(e) = std::fs::create_dir_all(parent) {
        warn!(
            "could not create poster cache dir {}: {e}",
            parent.display()
        );
        return;
    }
    if let Err(e) = std::fs::write(&path, bytes) {
        warn!("could not write poster cache to {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err() -> Result<PosterBytes, String> {
        Err("offline".to_string())
    }

    #[test]
    fn fresh_cache_is_used_without_fetching() {
        let res = resolve_remote(Some((vec![1, 2, 3], CacheFreshness::Fresh)), || {
            panic!("must not fetch on a fresh cache hit")
        });
        assert!(matches!(res, Resolution::CacheHit(b) if b == vec![1, 2, 3]));
    }

    #[test]
    fn miss_then_successful_fetch_is_persisted() {
        let res = resolve_remote(None, || Ok(vec![9, 9]));
        assert!(matches!(res, Resolution::Fetched(b) if b == vec![9, 9]));
    }

    #[test]
    fn stale_cache_prefers_a_fresh_fetch() {
        let res = resolve_remote(Some((vec![1], CacheFreshness::Stale)), || Ok(vec![2]));
        assert!(matches!(res, Resolution::Fetched(b) if b == vec![2]));
    }

    #[test]
    fn stale_cache_falls_back_when_fetch_fails_offline() {
        // The regression behind #64: an expired poster cache must still render
        // its thumbnail when the network is unreachable.
        let res = resolve_remote(Some((vec![7, 7], CacheFreshness::Stale)), err);
        assert!(matches!(res, Resolution::StaleFallback(b, _) if b == vec![7, 7]));
    }

    #[test]
    fn miss_with_failed_fetch_yields_no_poster() {
        let res = resolve_remote(None, err);
        assert!(matches!(res, Resolution::Failed(_)));
    }
}
