//! Poster prefetch for the browse UI.
//!
//! Posters are loaded once at startup. Local posters are read from disk;
//! remote posters are fetched over HTTP with a per-poster timeout. A failure
//! produces a placeholder rather than aborting the launch.

use std::collections::HashMap;
use std::time::Duration;

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
            PosterRef::Remote(url) => match agent.get(url.as_str()).call() {
                Ok(resp) => {
                    let mut buf = Vec::new();
                    if let Err(e) = std::io::copy(&mut resp.into_reader(), &mut buf) {
                        warn!(item = item.id, url = %url, "remote poster read failed: {e}");
                    } else {
                        debug!(item = item.id, bytes = buf.len(), "fetched remote poster");
                        cache.insert(item.id.clone(), buf);
                    }
                }
                Err(e) => {
                    warn!(item = item.id, url = %url, "remote poster fetch failed: {e}");
                }
            },
        }
    }

    cache
}
