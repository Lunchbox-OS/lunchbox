//! Filesystem paths shared across the Linux binary.

use std::path::PathBuf;

/// The `shepherd/media/<leaf>` directory under the user's cache home
/// (`$XDG_CACHE_HOME`, falling back to `$HOME/.cache`). Returns `None` when
/// neither environment variable is set, in which case the caller skips caching.
pub fn media_cache_dir(leaf: &str) -> Option<PathBuf> {
    let cache_home = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(cache_home.join("shepherd").join("media").join(leaf))
}

/// The `shepherd/media/<leaf>` directory under the user's state home
/// (`$XDG_STATE_HOME`, falling back to `$HOME/.local/state`). Returns `None`
/// when neither environment variable is set, in which case the caller does
/// without persistence.
///
/// Distinct from [`media_cache_dir`] on purpose: cached videos and posters can
/// be deleted at any time and simply re-downloaded, whereas the resume
/// positions kept here are only re-creatable by watching everything again.
pub fn media_state_dir(leaf: &str) -> Option<PathBuf> {
    let state_home = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("state"))
        })?;
    Some(state_home.join("shepherd").join("media").join(leaf))
}
