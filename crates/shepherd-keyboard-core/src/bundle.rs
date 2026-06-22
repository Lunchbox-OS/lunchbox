//! Bundle loading and trust anchoring (fail-closed).
//!
//! The decoder verifies a bundle's minisign signature and per-file hashes on load. This
//! module's only jobs are (1) to resolve the per-profile bundle directory, (2) to choose
//! the trust anchor (the committed dev key, or a caller-supplied production key that lives
//! somewhere a child cannot rewrite), and (3) to surface every failure as an [`Error`] so
//! the caller can degrade to tap-only rather than ever using an unverified decoder.

use std::path::{Path, PathBuf};

use shepherd_swipe_core::Decoder;

use crate::error::{Error, Result};
use crate::profile::Profile;

/// Resolve the bundle directory for a profile under a bundle *root*.
///
/// Release artifacts extract to `<root>/adult` and `<root>/child`, so a single root
/// (e.g. `dev-runtime/swipe-bundles`) holds both profiles' bundles.
pub fn bundle_dir(root: &Path, profile: Profile) -> PathBuf {
    root.join(profile.id())
}

/// Load a verified decoder from a bundle directory.
///
/// `public_key`, when `Some`, is the trusted minisign public key the bundle must verify
/// against (the production trust anchor). When `None`, the decoder verifies against the
/// committed dev key — appropriate for development and CI, never for a shipped child
/// session. Returns [`Error`] (never panics) on a missing directory or any verification /
/// compatibility failure; the caller must fail closed on `Err`.
pub fn load_decoder(dir: &Path, public_key: Option<&str>) -> Result<Decoder> {
    if !dir.is_dir() {
        return Err(Error::BundleMissing(dir.to_path_buf()));
    }
    let decoder = match public_key {
        Some(key) => Decoder::load_with_key(dir, key)?,
        None => Decoder::load(dir)?,
    };
    Ok(decoder)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_dir_appends_profile_id() {
        let root = Path::new("/var/lib/shepherd/swipe-bundles");
        assert_eq!(
            bundle_dir(root, Profile::Adult),
            Path::new("/var/lib/shepherd/swipe-bundles/adult")
        );
        assert_eq!(
            bundle_dir(root, Profile::Child),
            Path::new("/var/lib/shepherd/swipe-bundles/child")
        );
    }

    #[test]
    fn missing_directory_fails_closed() {
        // `Decoder` is not `Debug`, so match rather than `unwrap_err`.
        match load_decoder(Path::new("/nonexistent/swipe/bundle"), None) {
            Err(Error::BundleMissing(_)) => {}
            Ok(_) => panic!("a missing directory must not load a decoder"),
            Err(other) => panic!("expected BundleMissing, got {other}"),
        }
    }

    #[test]
    fn unsigned_or_empty_bundle_fails_closed() {
        // An existing directory that is not a valid signed bundle must error, never load.
        let dir = tempfile::tempdir().unwrap();
        match load_decoder(dir.path(), None) {
            Err(Error::Decoder(_)) => {}
            Ok(_) => panic!("an unsigned/empty bundle must fail closed, not load"),
            Err(other) => panic!("expected Decoder error, got {other}"),
        }
    }
}
