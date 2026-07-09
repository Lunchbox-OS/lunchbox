//! Run the Android UI on the host for fast iteration:
//!
//! ```sh
//! cargo run -p shepherd-media-android --example desktop_preview
//! ```
//!
//! Settings persist to a `shepherd-media-preview/settings.toml` under the
//! system temp dir so repeated runs keep their state without touching any real
//! config.

use std::path::PathBuf;

use shepherd_media_android::MediaApp;

fn main() -> eframe::Result<()> {
    let dir = std::env::temp_dir().join("shepherd-media-preview");
    std::fs::create_dir_all(&dir).ok();
    let settings_path: PathBuf = dir.join("settings.toml");
    let cache_dir: PathBuf = dir.join("cache");

    eframe::run_native(
        "shepherd-media (preview)",
        eframe::NativeOptions::default(),
        Box::new(move |cc| Ok(Box::new(MediaApp::new(cc, settings_path, cache_dir)))),
    )
}
