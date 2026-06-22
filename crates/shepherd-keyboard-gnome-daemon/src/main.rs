//! GNOME swipe-keyboard decode daemon.
//!
//! A headless D-Bus service over `shepherd-keyboard-core`. The GJS Shell extension captures
//! gestures and commits text through GNOME's input-method object; it calls this daemon for
//! candidates so both GNOME and wlroots decode through the identical core path.

mod service;

use std::path::PathBuf;

use anyhow::Context as _;
use clap::Parser;

use shepherd_keyboard_core::{Decoder, Profile, bundle};

use crate::service::SwipeDecoder;

const DBUS_NAME: &str = "com.armeafamily.ShepherdSwipe";
const DBUS_PATH: &str = "/com/armeafamily/ShepherdSwipe";

/// CLI for the GNOME decode daemon.
#[derive(Parser, Debug)]
#[command(name = "shepherd-keyboard-gnome-daemon")]
struct Args {
    /// Safety profile to load (adult | child).
    #[arg(long, default_value = "adult")]
    profile: String,
    /// Bundle root directory (contains `adult/` and `child/`).
    #[arg(long, env = "SHEPHERD_SWIPE_BUNDLE_DIR")]
    bundle_dir: Option<PathBuf>,
    /// Trusted minisign public key file (production trust anchor). Uses the decoder's
    /// committed dev key when unset.
    #[arg(long)]
    public_key: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args = Args::parse();

    let profile = Profile::parse(&args.profile).unwrap_or(Profile::Adult);
    let decoder = load_decoder(&args, profile);
    let service = SwipeDecoder::new(decoder, profile.to_string());

    let _conn = zbus::connection::Builder::session()
        .context("connect to the session bus")?
        .name(DBUS_NAME)
        .context("request the well-known name")?
        .serve_at(DBUS_PATH, service)
        .context("export the decode interface")?
        .build()
        .await
        .context("build the D-Bus connection")?;
    tracing::info!(name = DBUS_NAME, path = DBUS_PATH, %profile, "decode daemon ready");

    // Serve until killed.
    std::future::pending::<()>().await;
    Ok(())
}

/// Load the profile's signed bundle, failing closed (returns `None`) on any error so the
/// daemon still serves and simply returns no candidates.
fn load_decoder(args: &Args, profile: Profile) -> Option<Decoder> {
    let root = args
        .bundle_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from("dev-runtime/swipe-bundles"));
    let dir = bundle::bundle_dir(&root, profile);
    let public_key = match &args.public_key {
        Some(path) => match std::fs::read_to_string(path) {
            Ok(k) => Some(k),
            Err(e) => {
                tracing::error!(error = %e, "failed to read public key; failing closed");
                return None;
            }
        },
        None => None,
    };
    match bundle::load_decoder(&dir, public_key.as_deref()) {
        Ok(decoder) => {
            tracing::info!(%profile, dir = %dir.display(), "loaded signed bundle");
            Some(decoder)
        }
        Err(e) => {
            tracing::warn!(%profile, dir = %dir.display(), error = %e,
                "failed to load bundle; serving with no predictions (tap-only)");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Shared fixture: the same gesture the core and wlroots paths decode, so this asserts
    // cross-backend parity at the daemon boundary.
    const HELLO_GESTURE: &str =
        include_str!("../../shepherd-keyboard-core/tests/fixtures/hello.gesture.json");

    fn adult_decoder() -> Decoder {
        let root = std::env::var_os("SHEPHERD_SWIPE_BUNDLE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .parent()
                    .and_then(|p| p.parent())
                    .map(|repo| repo.join("dev-runtime/swipe-bundles"))
                    .expect("repo root")
            });
        let dir = bundle::bundle_dir(&root, Profile::Adult);
        bundle::load_decoder(&dir, None)
            .unwrap_or_else(|e| panic!("load adult bundle at {}: {e}", dir.display()))
    }

    #[test]
    #[ignore = "requires a downloaded signed bundle; run scripts/fetch-swipe-bundles.sh first"]
    fn daemon_decode_matches_core_on_hello_fixture() {
        let svc = SwipeDecoder::new(Some(adult_decoder()), "adult".to_string());
        let words = svc.decode_words(HELLO_GESTURE, "");
        assert!(!words.is_empty(), "daemon returned no candidates");
        assert_eq!(
            words[0].0, "hello",
            "daemon top candidate should match the core path ('hello'), got {words:?}"
        );
    }
}
