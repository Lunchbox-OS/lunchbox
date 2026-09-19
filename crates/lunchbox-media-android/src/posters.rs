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

use std::io::Read;
use std::path::PathBuf;

use lunchbox_media_app::PosterPolicy;
use lunchbox_media_app::RemotePosterCache;
use lunchbox_media_app::poster_cache::DEFAULT_TTL;
use lunchbox_media_core::PosterRef;

/// Largest poster we'll pull over HTTP, to bound memory on a tiny device.
const MAX_POSTER_BYTES: u64 = 8 * 1024 * 1024;

/// Whether a poster should be loaded at all under the given policy.
pub fn should_load(policy: PosterPolicy) -> bool {
    match policy {
        PosterPolicy::Never => false,
        // TODO: gate WifiOnly on a metered-connection check via JNI.
        PosterPolicy::Always | PosterPolicy::WifiOnly => true,
    }
}

/// Poster loader for the browse grid. Local posters are read straight from
/// their path; remote posters go through the shared [`RemotePosterCache`]
/// (URL-hash keyed, TTL + offline stale-fallback), with the capped download and
/// image decoding added here. Cheap to clone so each worker gets its own handle.
#[derive(Clone)]
pub struct PosterCache {
    remote: RemotePosterCache,
}

impl PosterCache {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            remote: RemotePosterCache::new(dir, DEFAULT_TTL),
        }
    }

    /// Read poster bytes, using the disk cache for remote posters. Blocking;
    /// call off the UI thread.
    pub fn load(&self, poster: &PosterRef) -> Option<Vec<u8>> {
        match poster {
            PosterRef::Local(path) => std::fs::read(path).ok(),
            PosterRef::Remote(url) => {
                let url = url.as_str();
                self.remote.load(url, || http_get_bytes(url))
            }
        }
    }

    /// Read and decode in one call (the worker-thread entry point).
    pub fn load_and_decode(&self, poster: &PosterRef) -> Option<egui::ColorImage> {
        decode(&self.load(poster)?)
    }
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
        let dir = std::env::temp_dir().join(format!("lunchbox-media-poster-{name}"));
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

    // The remote disk-cache behavior (fresh hit, stale offline fallback, TTL)
    // now lives in and is tested by lunchbox-media-app's RemotePosterCache.
}
