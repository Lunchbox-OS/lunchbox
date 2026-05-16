//! Poster prefetch for the browse UI.
//!
//! Posters are loaded once at startup. Local posters are read from disk;
//! remote posters are served from an on-disk cache at
//! `$XDG_CACHE_HOME/shepherd/media/posters/<hash>.bin` and only fetched over
//! HTTP on cache miss. A failure produces a placeholder rather than aborting
//! the launch; failed fetches are not cached so a transient network blip on
//! first launch doesn't become a permanent placeholder.

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
                if let Some(bytes) = load_from_disk_cache(url_str) {
                    debug!(item = item.id, url = %url, "poster disk cache hit");
                    cache.insert(item.id.clone(), bytes);
                    continue;
                }
                match agent.get(url_str).call() {
                    Ok(resp) => {
                        let mut buf = Vec::new();
                        if let Err(e) = std::io::copy(&mut resp.into_reader(), &mut buf) {
                            warn!(item = item.id, url = %url, "remote poster read failed: {e}");
                        } else {
                            debug!(item = item.id, bytes = buf.len(), "fetched remote poster");
                            save_to_disk_cache(url_str, &buf);
                            cache.insert(item.id.clone(), buf);
                        }
                    }
                    Err(e) => {
                        warn!(item = item.id, url = %url, "remote poster fetch failed: {e}");
                    }
                }
            }
        }
    }

    cache
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

fn load_from_disk_cache(url: &str) -> Option<PosterBytes> {
    let path = cache_path(url)?;
    let meta = std::fs::metadata(&path).ok()?;
    let modified = meta.modified().ok()?;
    if modified.elapsed().ok()? >= Duration::from_secs(CACHE_TTL_SECS) {
        debug!("poster cache stale for {url}");
        return None;
    }
    std::fs::read(&path).ok()
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
