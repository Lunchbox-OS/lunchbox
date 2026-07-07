//! Keep the TV awake while a video is on screen.
//!
//! With `vo=libmpv` the player owns no window, so mpv's own screensaver
//! inhibition never runs and the Fire TV dims/blanks mid-playback. Toggle
//! Android's `KEEP_SCREEN_ON` window flag ourselves instead.
//! [`android_activity::AndroidApp::set_window_flags`] marshals to the UI thread
//! (`ANativeActivity_setWindowFlags`), so it is safe to call from the render
//! loop. No-op on the host.

#[cfg(target_os = "android")]
use std::cell::RefCell;

#[cfg(target_os = "android")]
thread_local! {
    /// The `AndroidApp` handle, set once from `android_main`. Thread-local
    /// because the render loop that toggles the flag runs on that same thread.
    static APP: RefCell<Option<android_activity::AndroidApp>> = const { RefCell::new(None) };
}

/// Record the `AndroidApp` so [`keep_awake`] can reach the window. Call from
/// `android_main`.
#[cfg(target_os = "android")]
pub fn set_app(app: android_activity::AndroidApp) {
    APP.with(|slot| *slot.borrow_mut() = Some(app));
}

/// Add (`on`) or clear the `KEEP_SCREEN_ON` window flag. No-op on the host.
#[cfg(target_os = "android")]
pub fn keep_awake(on: bool) {
    use android_activity::WindowManagerFlags;
    APP.with(|slot| {
        if let Some(app) = slot.borrow().as_ref() {
            let flag = WindowManagerFlags::KEEP_SCREEN_ON;
            let (add, remove) = if on {
                (flag, WindowManagerFlags::empty())
            } else {
                (WindowManagerFlags::empty(), flag)
            };
            app.set_window_flags(add, remove);
        }
    });
}

#[cfg(not(target_os = "android"))]
pub fn keep_awake(_on: bool) {}
