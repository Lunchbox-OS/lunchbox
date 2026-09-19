//! Android build of `lunchbox-media`.
//!
//! This crate is the `cdylib` the APK loads. It hosts the cross-platform egui
//! UI ([`MediaApp`]) over the shared `lunchbox-media-core` and the
//! `lunchbox-media-app` settings layer. The libmpv-backed player and the
//! source-resolution/JNI bridges (SAF, connectivity, yt-dlp) are added in later
//! steps; see the design doc under `docs/ai/history`.
//!
//! On Android, `android_main` (below) is the entry point invoked by the
//! `NativeActivity` glue. On the host, the same [`MediaApp`] runs via the
//! `desktop_preview` example for fast UI iteration.

pub mod exit;
pub mod ffmpeg;
pub mod handoff;
pub mod insets;
pub mod playback;
pub mod player;
pub mod posters;
pub mod resolve;
pub mod sponsorblock;
pub mod storage;
pub mod surface;
pub mod ui;
pub mod video_cache;
pub mod youtube;

pub use player::StubPlayer;
pub use ui::MediaApp;

/// Android entry point, called by the native-activity glue once the .so loads.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: android_activity::AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Info),
    );

    // Give FFmpeg the JavaVM before any player exists — without it MediaCodec
    // cannot be reached and every frame decodes on the CPU.
    ffmpeg::register_java_vm();

    // Record the activity handle so the safe-area inset query can reach
    // getWindow()/getRootWindowInsets() and the file browser can reach a
    // Context (ndk_context's context is the Application, which has no window).
    insets::set_activity(app.activity_as_ptr());
    storage::set_activity(app.activity_as_ptr());
    // So BACK from the top-level screen can finish the activity and exit.
    exit::set_activity(app.activity_as_ptr());
    // The video SurfaceView mpv decodes into lives on the activity too.
    surface::set_activity(app.activity_as_ptr());

    // Keep the TV awake while the app is foreground. With `vo=libmpv` there is no
    // player window to inhibit the screensaver, so it would blank mid-video. Set
    // the flag once here, before the eframe loop starts: `set_window_flags` takes
    // android-activity's activity lock, which the render loop also holds while
    // dispatching input/redraw — calling it from inside `App::update` deadlocks
    // (ANR). Nothing holds that lock yet at startup, so this is safe.
    app.set_window_flags(
        android_activity::WindowManagerFlags::KEEP_SCREEN_ON,
        android_activity::WindowManagerFlags::empty(),
    );

    // Persist settings and caches in the app's private storage.
    let data_dir = app
        .internal_data_path()
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let settings_path = data_dir.join("settings.toml");
    let cache_dir = data_dir.join("cache");

    let options = eframe::NativeOptions {
        android_app: Some(app),
        // Ask glutin for an EGL config with an alpha channel. Without it the
        // window has no alpha to be transparent *with*, and the video
        // SurfaceView behind it never shows through — see `clear_color` in
        // `ui.rs` and the translucent window declared in Theme.ShepherdMedia.
        viewport: egui::ViewportBuilder::default().with_transparent(true),
        ..Default::default()
    };

    if let Err(e) = eframe::run_native(
        "lunchbox-media",
        options,
        Box::new(move |cc| Ok(Box::new(MediaApp::new(cc, settings_path, cache_dir)))),
    ) {
        log::error!("eframe exited with error: {e}");
    }
}
