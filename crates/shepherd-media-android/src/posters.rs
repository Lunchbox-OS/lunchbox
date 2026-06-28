//! Poster loading for the browse grid.
//!
//! Posters are loaded off the UI thread (network must not touch the Android UI
//! thread, and decoding is CPU work): a worker reads the bytes (local file or
//! HTTP) and decodes them to an [`egui::ColorImage`], which the UI thread then
//! uploads as a texture.
//!
//! [`PosterPolicy::Never`] skips loading entirely. `WifiOnly` currently behaves
//! like `Always`; gating it on a metered/Wi-Fi connection needs the Android
//! connectivity JNI bridge, which is not wired yet.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use shepherd_media_app::PosterPolicy;
use shepherd_media_core::PosterRef;

/// Largest poster we'll pull over HTTP, to bound memory on a tiny device.
const MAX_POSTER_BYTES: u64 = 8 * 1024 * 1024;

/// How long a cached remote poster is considered fresh. A stale entry is still
/// used as an offline fallback when a refresh fails (mirrors the Linux binary).
const CACHE_TTL: Duration = Duration::from_secs(6 * 3600);

/// Whether a poster should be loaded at all under the given policy.
pub fn should_load(policy: PosterPolicy) -> bool {
    match policy {
        PosterPolicy::Never => false,
        // TODO: gate WifiOnly on a metered-connection check via JNI.
        PosterPolicy::Always | PosterPolicy::WifiOnly => true,
    }
}

/// On-disk cache for remote posters. Local posters are read straight from their
/// path and never cached. Cheap to clone (just a dir + TTL) so each worker
/// thread gets its own handle.
#[derive(Clone)]
pub struct PosterCache {
    dir: PathBuf,
    ttl: Duration,
}

impl PosterCache {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            ttl: CACHE_TTL,
        }
    }

    /// Read poster bytes, using the disk cache for remote posters. Blocking;
    /// call off the UI thread.
    pub fn load(&self, poster: &PosterRef) -> Option<Vec<u8>> {
        match poster {
            PosterRef::Local(path) => std::fs::read(path).ok(),
            PosterRef::Remote(url) => self.load_remote(url.as_str()),
        }
    }

    /// Read and decode in one call (the worker-thread entry point).
    pub fn load_and_decode(&self, poster: &PosterRef) -> Option<egui::ColorImage> {
        decode(&self.load(poster)?)
    }

    fn load_remote(&self, url: &str) -> Option<Vec<u8>> {
        let path = self.path_for(url);
        let cached = read_with_age(&path);

        // Fresh cache hit: use it without touching the network.
        if let Some((bytes, age)) = &cached
            && *age < self.ttl
        {
            return Some(bytes.clone());
        }

        // Stale or missing: try a fresh fetch, falling back to the stale copy.
        match http_get_bytes(url) {
            Some(bytes) => {
                self.write(&path, &bytes);
                Some(bytes)
            }
            None => cached.map(|(bytes, _)| bytes),
        }
    }

    fn path_for(&self, url: &str) -> PathBuf {
        let mut h = DefaultHasher::new();
        url.hash(&mut h);
        self.dir.join(format!("{:016x}.bin", h.finish()))
    }

    fn write(&self, path: &Path, bytes: &[u8]) {
        let _ = std::fs::create_dir_all(&self.dir);
        let _ = std::fs::write(path, bytes);
    }
}

/// Read a cached file with its age, or `None` if it doesn't exist.
fn read_with_age(path: &Path) -> Option<(Vec<u8>, Duration)> {
    let bytes = std::fs::read(path).ok()?;
    let age = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .unwrap_or(Duration::ZERO);
    Some((bytes, age))
}

fn http_get_bytes(url: &str) -> Option<Vec<u8>> {
    let resp = ureq::get(url).call().ok()?;
    let mut buf = Vec::new();
    resp.into_reader()
        .take(MAX_POSTER_BYTES)
        .read_to_end(&mut buf)
        .ok()?;
    Some(buf)
}

/// Decode encoded image bytes (JPEG/PNG/WebP) into an egui image. Pure CPU; safe
/// to run on a worker thread.
pub fn decode(bytes: &[u8]) -> Option<egui::ColorImage> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let size = [img.width() as usize, img.height() as usize];
    Some(egui::ColorImage::from_rgba_unmultiplied(
        size,
        img.as_flat_samples().as_slice(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a `w`×`h` red PNG using the same `image` crate `decode` uses.
    fn red_png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([255, 0, 0, 255]));
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();
        buf.into_inner()
    }

    #[test]
    fn never_policy_skips() {
        assert!(!should_load(PosterPolicy::Never));
        assert!(should_load(PosterPolicy::Always));
        assert!(should_load(PosterPolicy::WifiOnly));
    }

    #[test]
    fn decodes_png() {
        let img = decode(&red_png(2, 3)).expect("valid png decodes");
        assert_eq!(img.size, [2, 3]);
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(decode(b"not an image").is_none());
    }

    fn unique_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("shepherd-media-poster-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn loads_local_poster() {
        let dir = unique_dir("local");
        let path = dir.join("p.png");
        std::fs::write(&path, red_png(4, 4)).unwrap();
        let cache = PosterCache::new(dir.join("cache"));
        let img = cache
            .load_and_decode(&PosterRef::Local(path))
            .expect("local poster loads and decodes");
        assert_eq!(img.size, [4, 4]);
    }

    #[test]
    fn fresh_cache_entry_is_a_hit() {
        let dir = unique_dir("hit");
        let cache = PosterCache::new(dir.clone());
        // Pre-seed the cache file for a URL; a fresh entry must be returned
        // without any network access (this URL is never reachable in tests).
        let url = "https://unreachable.invalid/poster.png";
        let path = cache.path_for(url);
        let bytes = red_png(5, 5);
        cache.write(&path, &bytes);

        let loaded = cache
            .load(&PosterRef::Remote(url.parse().unwrap()))
            .expect("fresh cache hit");
        assert_eq!(loaded, bytes);
    }

    #[test]
    fn read_with_age_reports_missing() {
        assert!(read_with_age(Path::new("/nonexistent/poster.bin")).is_none());
    }
}
