//! Android build of `shepherd-media`.
//!
//! This crate is the `cdylib` the APK loads. It hosts the cross-platform egui
//! UI ([`MediaApp`]) over the shared `shepherd-media-core` and the
//! `shepherd-media-app` settings layer. The libmpv-backed player and the
//! source-resolution/JNI bridges (SAF, connectivity, yt-dlp) are added in later
//! steps; see the design doc under `docs/ai/history`.
//!
//! On Android, `android_main` (below) is the entry point invoked by the
//! `NativeActivity` glue. On the host, the same [`MediaApp`] runs via the
//! `desktop_preview` example for fast UI iteration.

pub mod playback;
pub mod player;
pub mod posters;
pub mod resolve;
pub mod ui;

pub use player::StubPlayer;
pub use ui::MediaApp;

/// Android entry point, called by the native-activity glue once the .so loads.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: android_activity::AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Info),
    );

    // Persist settings in the app's private storage.
    let settings_path = app
        .internal_data_path()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("settings.toml");

    let options = eframe::NativeOptions {
        android_app: Some(app),
        ..Default::default()
    };

    if let Err(e) = eframe::run_native(
        "shepherd-media",
        options,
        Box::new(move |cc| Ok(Box::new(MediaApp::new(cc, settings_path)))),
    ) {
        log::error!("eframe exited with error: {e}");
    }
}
