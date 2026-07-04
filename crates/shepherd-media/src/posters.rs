//! Poster prefetch for the browse UI.
//!
//! Posters are loaded once at startup. Local posters are read from disk;
//! remote posters are served from an on-disk cache under
//! `$XDG_CACHE_HOME/shepherd/media/posters/` and only fetched over HTTP on a
//! cache miss or when the cached copy has expired. A failure produces a
//! placeholder rather than aborting the launch; failed fetches are not cached
//! so a transient network blip on first launch doesn't become a permanent
//! placeholder.
//!
//! The disk cache itself — URL hashing, TTL freshness, write-back, and the
//! offline stale-fallback (a stale entry is still served when a refresh fails,
//! so a device offline past the TTL keeps its thumbnails) — is the shared
//! [`RemotePosterCache`] from `shepherd-media-app`, used identically by the
//! Android app. Only the HTTP fetch and the XDG cache-dir choice are here.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use shepherd_media_app::poster_cache::{DEFAULT_TTL, RemotePosterCache, Resolution};
use shepherd_media_core::{Library, PosterRef};
use tracing::{debug, warn};

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
    // The shared disk cache. `None` only when no cache home can be determined
    // (no `$XDG_CACHE_HOME` and no `$HOME`), in which case posters are fetched
    // every launch without caching.
    let disk = poster_cache_dir().map(|d| RemotePosterCache::new(d, DEFAULT_TTL));

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
                let bytes = match &disk {
                    Some(disk) => match disk.resolve(url_str, || fetch_remote(&agent, url_str)) {
                        Resolution::Fresh(bytes) => {
                            debug!(item = item.id, url = %url, "poster disk cache hit");
                            Some(bytes)
                        }
                        Resolution::Fetched(bytes) => {
                            debug!(item = item.id, bytes = bytes.len(), "fetched remote poster");
                            Some(bytes)
                        }
                        Resolution::Stale(bytes, e) => {
                            warn!(item = item.id, url = %url, "remote poster fetch failed: {e}; using stale cache");
                            Some(bytes)
                        }
                        Resolution::Miss(e) => {
                            warn!(item = item.id, url = %url, "remote poster fetch failed: {e}");
                            None
                        }
                    },
                    None => match fetch_remote(&agent, url_str) {
                        Ok(bytes) => Some(bytes),
                        Err(e) => {
                            warn!(item = item.id, url = %url, "remote poster fetch failed: {e}");
                            None
                        }
                    },
                };
                if let Some(bytes) = bytes {
                    cache.insert(item.id.clone(), bytes);
                }
            }
        }
    }

    cache
}

/// Fetch poster bytes over HTTP, returning an error string on any failure.
fn fetch_remote(agent: &ureq::Agent, url: &str) -> Result<PosterBytes, String> {
    let resp = agent.get(url).call().map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    std::io::copy(&mut resp.into_reader(), &mut buf).map_err(|e| e.to_string())?;
    Ok(buf)
}

/// The on-disk poster cache directory, or `None` if no cache home is known.
fn poster_cache_dir() -> Option<PathBuf> {
    let cache_home = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(cache_home.join("shepherd").join("media").join("posters"))
}
