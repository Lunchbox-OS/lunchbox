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

use shepherd_media_app::PosterPolicy;
use shepherd_media_core::PosterRef;

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

/// Read poster bytes from disk or over HTTP. Blocking; call off the UI thread.
pub fn load_bytes(poster: &PosterRef) -> Option<Vec<u8>> {
    match poster {
        PosterRef::Local(path) => std::fs::read(path).ok(),
        PosterRef::Remote(url) => http_get_bytes(url.as_str()),
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

/// Convenience: read and decode in one call (the worker-thread entry point).
pub fn load_and_decode(poster: &PosterRef) -> Option<egui::ColorImage> {
    decode(&load_bytes(poster)?)
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

    #[test]
    fn loads_local_poster() {
        let dir = std::env::temp_dir().join("shepherd-media-poster-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("p.png");
        std::fs::write(&path, red_png(4, 4)).unwrap();
        let poster = PosterRef::Local(path);
        let img = load_and_decode(&poster).expect("local poster loads and decodes");
        assert_eq!(img.size, [4, 4]);
    }
}
