//! Cache keys.
//!
//! Two keys, deliberately, because storage and interest are not the same thing.
//!
//! - A **content key** — [`content_key`] — names the files on disk. It hashes
//!   the source URL *and* the yt-dlp format selector, so the same video at two
//!   qualities is two files that coexist. Before the selector was in the key,
//!   two media entries over one library with different `quality` settings
//!   deleted and re-downloaded each other's copy on every launch (issue #127).
//! - An **interest key** — [`interest_key`] — hashes the URL alone, and names
//!   the `.played` marker. A child who watched a video has shown interest in
//!   the *video*, not in a particular rendition of it.
//!
//! Neither is the library item id. Item ids are unique only within a library,
//! and the cache is one flat directory shared by every library on the device:
//! two libraries that both define `intro` would otherwise share a file, and
//! whichever downloaded first would be served to both. Hashing the URL also
//! dedupes — one video referenced twice is downloaded once.

use sha2::{Digest, Sha256};

use shepherd_media_core::{ClassifiedUri, Source};

/// Hex characters kept from a digest. 128 bits is far past what a
/// family-sized cache can collide in, and a short name keeps a `ls` of the
/// cache directory readable.
const KEY_LEN: usize = 32;

/// The URL a remote `Source` is fetched from, or `None` for a local file that
/// needs no caching.
pub fn source_url(source: &Source) -> Option<String> {
    match &source.uri {
        ClassifiedUri::YouTube(url)
        | ClassifiedUri::DirectHttp(url)
        | ClassifiedUri::Unknown(url) => Some(url.to_string()),
        ClassifiedUri::Local(_) => None,
    }
}

/// Truncated hex SHA-256 of `parts`, joined by a NUL that cannot appear in a
/// URL or a selector — so no two distinct inputs can produce the same
/// pre-image.
///
/// Stable across runs, processes, and toolchain upgrades, which is why this is
/// not `DefaultHasher`: its output is explicitly unspecified and a Rust upgrade
/// would silently orphan the entire cache.
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
/// A direct-HTTP download picks no format and passes an empty selector, so its
/// key is unaffected by a change to the yt-dlp one.
pub fn content_key(url: &str, selector: &str) -> String {
    hash_parts(&[url, selector])
}

/// The key naming the `.played` marker for `url` — one per video, whatever
/// quality it was fetched at.
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

    #[test]
    fn different_urls_get_different_keys() {
        assert_ne!(
            content_key("https://example.com/a.mp4", H264),
            content_key("https://example.com/b.mp4", H264)
        );
    }

    /// The bug the selector-in-key exists to prevent: two entries over one
    /// library at different qualities used to delete each other's download.
    #[test]
    fn two_qualities_of_one_video_coexist() {
        assert_ne!(
            content_key("https://example.com/a.mp4", H264),
            content_key("https://example.com/a.mp4", LOW),
            "each quality must get its own file rather than replace the other"
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

    /// The other collision this keying exists to prevent: two libraries
    /// defining the same item id must not share a cache file.
    #[test]
    fn the_same_item_id_in_two_libraries_does_not_collide() {
        assert_ne!(
            content_key("https://one.example/intro.mp4", H264),
            content_key("https://two.example/intro.mp4", H264)
        );
    }

    /// The separator matters: without it, ("ab", "c") and ("a", "bc") would
    /// hash the same, so a URL ending in the selector's first characters could
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
