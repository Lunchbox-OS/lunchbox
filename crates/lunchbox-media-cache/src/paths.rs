//! Filesystem paths for the media caches.

use std::path::PathBuf;

/// The `shepherd/media/<leaf>` directory under the user's cache home
/// (`$XDG_CACHE_HOME`, falling back to `$HOME/.cache`). Returns `None` when
/// neither environment variable is set, in which case the caller skips caching.
///
/// lunchboxd runs in the same user session as `lunchbox-media`, so both resolve
/// this to the same directory — which is what lets the daemon prefetch into the
/// cache the player reads.
pub fn media_cache_dir(leaf: &str) -> Option<PathBuf> {
    let cache_home = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(cache_home.join("shepherd").join("media").join(leaf))
}
