//! Stable names for cached media files, shared by both front-ends' video
//! caches.
//!
//! Two keys, because storage and interest are not the same thing:
//!
//! - A **content key** hashes the source URL *and* the format selector it was
//!   downloaded with, so the same video at two qualities is two files that
//!   coexist rather than deleting each other. A cache with no notion of quality
//!   (the Android one, which only ever caches plain HTTP sources) passes an
//!   empty selector.
//! - An **interest key** hashes the URL alone. Somebody who watched a video has
//!   shown interest in the *video*, not in a particular rendition of it.
//!
//! Neither is a library item id: item ids are unique only within a library, and
//! a cache directory serves every library on the device. Hashing the URL also
//! dedupes — one video referenced twice is downloaded once.
//!
//! **Not `DefaultHasher`.** Its output is explicitly unspecified and may change
//! between Rust releases, which would rename every file in the cache on a
//! toolchain bump: everything silently re-downloads, and the orphans sit there
//! occupying the size cap until eviction reaches them.

use sha2::{Digest, Sha256};

/// Hex characters kept from a digest. 128 bits is far past what a
/// family-sized cache can collide in, and a short name keeps a listing of the
/// cache directory readable.
pub const KEY_LEN: usize = 32;

/// Truncated hex SHA-256 of `parts`, joined by a NUL that cannot appear in a
/// URL or a selector — so no two distinct inputs share a pre-image.
fn hash_parts(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            hasher.update(b"\0");
        }
        hasher.update(part.as_bytes());
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(KEY_LEN);
    for byte in digest.iter() {
        if out.len() >= KEY_LEN {
            break;
        }
        out.push_str(&format!("{byte:02x}"));
    }
    out.truncate(KEY_LEN);
    out
}

/// The key naming the cached files for `url` downloaded under `selector`.
///
/// Pass `""` for a source that involves no format selection, such as a direct
/// HTTP download.
pub fn content_key(url: &str, selector: &str) -> String {
    hash_parts(&[url, selector])
}

/// The key naming the "this has been watched" marker for `url` — one per
/// video, whatever quality it was fetched at.
pub fn interest_key(url: &str) -> String {
    hash_parts(&[url])
}

#[cfg(test)]
mod tests {
    use super::*;

    const H264: &str = "bv*[vcodec^=avc1][height<=?1080]+ba/b";
    const LOW: &str = "bv*[vcodec^=avc1][height<=?480]+ba/b";

    #[test]
    fn keys_are_stable_and_hex() {
        let key = content_key("https://example.com/a.mp4", H264);
        assert_eq!(key.len(), KEY_LEN);
        assert!(key.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(key, content_key("https://example.com/a.mp4", H264));
    }

    /// Pinned so a change to the hash is a deliberate act, not a silent
    /// re-download of every cache on every device.
    #[test]
    fn keys_are_pinned_to_known_values() {
        assert_eq!(
            content_key("https://example.com/a.mp4", ""),
            "6afa20e49cad0181f0661bdce456d4d6"
        );
        assert_eq!(
            interest_key("https://example.com/a.mp4"),
            "0e06dca0234da29358bb3b0f700b1473"
        );
    }

    #[test]
    fn different_urls_get_different_keys() {
        assert_ne!(
            content_key("https://example.com/a.mp4", H264),
            content_key("https://example.com/b.mp4", H264)
        );
    }

    /// Two entries over one library at different qualities must not delete each
    /// other's download.
    #[test]
    fn two_qualities_of_one_video_coexist() {
        assert_ne!(
            content_key("https://example.com/a.mp4", H264),
            content_key("https://example.com/a.mp4", LOW)
        );
    }

    /// ...but they are the same *video*, so interest is shared.
    #[test]
    fn interest_is_per_video_not_per_quality() {
        assert_eq!(
            interest_key("https://example.com/a.mp4"),
            interest_key("https://example.com/a.mp4")
        );
        assert_ne!(
            interest_key("https://example.com/a.mp4"),
            interest_key("https://example.com/b.mp4")
        );
    }

    /// The collision the URL keying exists to prevent: two libraries defining
    /// the same item id must not share a cache file.
    #[test]
    fn the_same_item_id_in_two_libraries_does_not_collide() {
        assert_ne!(
            content_key("https://one.example/intro.mp4", H264),
            content_key("https://two.example/intro.mp4", H264)
        );
    }

    /// The separator matters: without it, ("ab", "c") and ("a", "bc") would
    /// hash alike, so a URL ending in the selector's first characters could
    /// collide with a shorter one.
    #[test]
    fn the_url_and_selector_cannot_run_together() {
        assert_ne!(content_key("ab", "c"), content_key("a", "bc"));
    }

    #[test]
    fn a_key_is_a_valid_filename_component() {
        let key = content_key("https://example.com/a b?c=d&e=/../f.mp4", H264);
        assert!(!key.contains('/'));
        assert!(!key.contains('.'));
    }
}
