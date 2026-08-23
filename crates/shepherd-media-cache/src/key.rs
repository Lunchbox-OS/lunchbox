//! Cache keys.
//!
//! The key derivation itself is `shepherd_media_app::cache_key`, shared with
//! the Android cache so the two agree on what a cached file is called and on
//! not using a hash whose output changes between Rust releases. Re-exported
//! here so callers of this crate have one place to look.
//!
//! What is local to this crate is [`source_url`]: turning a
//! `shepherd_media_core::Source` into the URL those keys hash. The shared crate
//! knows nothing about library sources.

pub use shepherd_media_app::cache_key::{content_key, interest_key};

use shepherd_media_core::{ClassifiedUri, Source};

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

#[cfg(test)]
mod tests {
    use super::*;
    use shepherd_media_core::PlayerHint;
    use std::path::PathBuf;

    fn source(uri: ClassifiedUri) -> Source {
        Source {
            platforms: Vec::new(),
            uri,
            player_hint: Some(PlayerHint::Mpv),
        }
    }

    #[test]
    fn a_local_source_has_no_url_to_hash() {
        assert!(source_url(&source(ClassifiedUri::Local(PathBuf::from("/a.mp4")))).is_none());
    }

    #[test]
    fn remote_sources_yield_their_url() {
        let url = "https://example.com/a.mp4";
        assert_eq!(
            source_url(&source(ClassifiedUri::DirectHttp(url.parse().unwrap()))).as_deref(),
            Some(url)
        );
    }
}
