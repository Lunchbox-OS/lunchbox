//! Quitting the app.
//!
//! BACK from the top-level library switcher should leave lunchbox-media, the way
//! a TV user expects BACK from a home screen to exit. eframe's
//! `ViewportCommand::Close` doesn't reliably finish a `NativeActivity` (winit
//! stops its event loop, but Android keeps the activity around), so we finish the
//! activity directly over JNI. No-op on the host, where the desktop preview
//! closes through the window manager.

/// Finish the Android `NativeActivity`, ending the app. No-op on the host.
#[cfg(target_os = "android")]
pub fn finish() {
    android::finish();
}

#[cfg(not(target_os = "android"))]
pub fn finish() {}

/// Record the `NativeActivity` instance (`AndroidApp::activity_as_ptr()`) so
/// [`finish`] can call `finish()` on it. Call from `android_main`.
#[cfg(target_os = "android")]
pub fn set_activity(activity: *mut core::ffi::c_void) {
    android::ACTIVITY.store(activity, std::sync::atomic::Ordering::Release);
}

#[cfg(not(target_os = "android"))]
pub fn set_activity(_activity: *mut core::ffi::c_void) {}

#[cfg(target_os = "android")]
mod android {
    use core::ffi::c_void;
    use jni::JavaVM;
    use jni::objects::JObject;
    use std::sync::atomic::{AtomicPtr, Ordering};

    /// The `NativeActivity` instance jobject, set once from `android_main`.
    pub(super) static ACTIVITY: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

    pub fn finish() {
        let ptr = ACTIVITY.load(Ordering::Acquire);
        if ptr.is_null() {
            return;
        }
        let ctx = ndk_context::android_context();
        let Ok(vm) = (unsafe { JavaVM::from_raw(ctx.vm().cast()) }) else {
            return;
        };
        let Ok(mut env) = vm.attach_current_thread() else {
            return;
        };
        // Don't inherit or leak a pending Java exception around the call.
        let _ = env.exception_clear();
        let activity = unsafe { JObject::from_raw(ptr.cast()) };
        // Finishes the activity and returns to the launcher. Android keeps the
        // process cached afterward, which is expected — this ends the app's UI,
        // it doesn't kill the process.
        if let Err(e) = env.call_method(&activity, "finish", "()V", &[]) {
            log::warn!("exit::finish: Activity.finish() failed: {e}");
        }
        let _ = env.exception_clear();
    }
}
