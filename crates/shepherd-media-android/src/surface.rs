//! The video `Surface` mpv decodes into.
//!
//! [`ShepherdMediaActivity`][activity] owns a `SurfaceView` behind the
//! activity's own window and exposes its `Surface` through
//! `getVideoSurface()`. This module fetches that over JNI and hands mpv the
//! jobject pointer as `--wid`, which is what lets it use
//! `vo=mediacodec_embed` + `hwdec=mediacodec` and keep decoded frames on the
//! GPU (see [`VideoOutput::AndroidSurface`]).
//!
//! [activity]: ../../android/app/src/main/java/com/armeafamily/shepherd/media/ShepherdMediaActivity.java
//!
//! ## Reference lifetime
//!
//! `getVideoSurface()` returns a *local* reference, which dies when the JNI
//! frame is popped — but mpv keeps the pointer for as long as the VO lives. So
//! [`acquire`] promotes it to a global reference and holds it, replacing it on
//! the next play rather than dropping it when playback ends. See the note below
//! on why nothing detaches on teardown.

use shepherd_media_core::PlayerHandle;

/// Attach the activity's video Surface to `player`.
///
/// **Every** play must be preceded by this. `vo=mediacodec_embed` does not
/// degrade when no window is set — it asserts `WinID != 0` and aborts the
/// process — so a missing Surface has to become an error the caller reports,
/// never a play that gets attempted anyway.
///
/// Off Android there is no Surface and no such VO, so this succeeds trivially
/// and the desktop preview keeps working.
pub fn attach(player: &mut dyn PlayerHandle) -> Result<(), String> {
    if !cfg!(target_os = "android") {
        return Ok(());
    }
    match acquire() {
        Some(handle) => {
            player.set_video_surface(Some(handle));
            Ok(())
        }
        None => Err("the video surface is not ready yet".to_string()),
    }
}

// There is deliberately no `detach`.
//
// Handing mpv `wid = -1` after `stop()` looks like the tidy thing to do, and it
// crashes: `stop()` only queues the teardown, so if the VO thread reconfigures
// before it finishes it re-enters `create_mediacodec_device_ref`, finds
// `WinID == -1`, and aborts the process. That surfaced as playback crashing
// roughly one time in four when leaving with the back button — a race, so it
// looked random.
//
// Nothing needs detaching anyway: the Surface belongs to the SurfaceView and
// outlives any single playback, and [`acquire`] replaces our reference on the
// next play. The one case that would need it — Android destroying the Surface
// while playback runs, i.e. backgrounding mid-video — wants playback stopped
// first, not the window pulled out from under a live VO.

/// Record the activity handle, as `insets`/`storage`/`exit` each do — the
/// Surface getter lives on the activity, and `ndk_context`'s context is the
/// Application, which has no view hierarchy.
#[cfg(target_os = "android")]
pub fn set_activity(activity: *mut core::ffi::c_void) {
    android::ACTIVITY.store(activity, std::sync::atomic::Ordering::Release);
}

#[cfg(not(target_os = "android"))]
pub fn set_activity(_activity: *mut core::ffi::c_void) {}

#[cfg(target_os = "android")]
pub use android::acquire;

/// Off Android there is no Surface; the desktop preview renders through the
/// stub player.
#[cfg(not(target_os = "android"))]
pub fn acquire() -> Option<i64> {
    None
}

#[cfg(target_os = "android")]
mod android {
    use core::ffi::c_void;
    use jni::JavaVM;
    use jni::objects::{GlobalRef, JObject};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicPtr, Ordering};

    /// The activity jobject, set once from `android_main`.
    pub(super) static ACTIVITY: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

    /// The global reference keeping the Surface alive while mpv holds its
    /// pointer. `None` whenever nothing is attached.
    static HELD: Mutex<Option<GlobalRef>> = Mutex::new(None);

    /// Fetch the activity's current video Surface and pin it, returning the
    /// jobject pointer for mpv's `--wid`.
    pub fn acquire() -> Option<i64> {
        // Drop any previous reference first: re-acquiring without releasing
        // would pin a Surface nothing is using any more.
        release();

        let activity_ptr = ACTIVITY.load(Ordering::Acquire);
        if activity_ptr.is_null() {
            return None;
        }
        let ctx = ndk_context::android_context();
        let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }.ok()?;
        let mut env = vm.attach_current_thread().ok()?;
        // Don't inherit a Java exception left pending by earlier JNI work.
        let _ = env.exception_clear();

        let activity = unsafe { JObject::from_raw(activity_ptr.cast()) };
        let result = env
            .call_method(
                &activity,
                "getVideoSurface",
                "()Landroid/view/Surface;",
                &[],
            )
            .and_then(|v| v.l());
        // Always clear, so a failure here can't abort the render loop's next
        // JNI call (the same discipline as insets.rs and storage.rs).
        let _ = env.exception_clear();

        let surface = match result {
            Ok(s) if !s.is_null() => s,
            Ok(_) => return None,
            Err(e) => {
                log::warn!("getVideoSurface() failed: {e}");
                return None;
            }
        };

        let global = match env.new_global_ref(&surface) {
            Ok(g) => g,
            Err(e) => {
                log::warn!("could not pin the video Surface: {e}");
                return None;
            }
        };
        let handle = global.as_obj().as_raw() as i64;
        *HELD.lock().unwrap() = Some(global);
        Some(handle)
    }

    /// Drop our reference to the Surface. Only ever called from `acquire`,
    /// which immediately takes a fresh one — see the note above about why
    /// nothing releases on teardown.
    fn release() {
        *HELD.lock().unwrap() = None;
    }
}
