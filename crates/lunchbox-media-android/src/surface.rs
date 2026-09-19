//! The video `Surface` mpv decodes into.
//!
//! [`LunchboxMediaActivity`][activity] owns a `SurfaceView` behind the
//! activity's own window and exposes its `Surface` through
//! `getVideoSurface()`. This module fetches that over JNI and hands mpv the
//! jobject pointer as `--wid`, which is what lets it use
//! `vo=mediacodec_embed` + `hwdec=mediacodec` and keep decoded frames on the
//! GPU (see [`VideoOutput::AndroidSurface`]).
//!
//! [activity]: ../../android/app/src/main/java/com/lunchboxos/media/LunchboxMediaActivity.java
//!
//! ## Reference lifetime
//!
//! `getVideoSurface()` returns a *local* reference, which dies when the JNI
//! frame is popped — but mpv keeps the pointer for as long as the VO lives. So
//! [`acquire`] promotes it to a global reference and holds it, replacing it on
//! the next play rather than dropping it when playback ends. See the note below
//! on why nothing detaches on teardown.

use lunchbox_media_core::PlayerHandle;

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

/// The rectangle a video of `video` size should occupy inside a `container` of
/// this size: as large as fits, centred, with the aspect ratio preserved.
///
/// In whatever unit both arguments are in — the caller uses window pixels for
/// the SurfaceView and egui points for the bars it paints around it.
pub fn fit_video(container: (f32, f32), video: (f32, f32)) -> Option<VideoRect> {
    let (cw, ch) = container;
    let (vw, vh) = video;
    if !(cw > 0.0 && ch > 0.0 && vw > 0.0 && vh > 0.0) {
        return None;
    }
    let (width, height) = if cw / ch > vw / vh {
        // The container is wider than the video: pillarbox.
        (ch * (vw / vh), ch)
    } else {
        // Taller: letterbox.
        (cw, cw * (vh / vw))
    };
    Some(VideoRect {
        x: (cw - width) / 2.0,
        y: (ch - height) / 2.0,
        width,
        height,
    })
}

/// Where the video goes, and by implication where the bars go.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VideoRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// Place the activity's video SurfaceView at `rect`, in window pixels.
///
/// Under `mediacodec_embed` the decoder scales its output to fill the Surface,
/// and `--keepaspect` never applies because nothing under mpv's control draws
/// the frame — so the Surface has to be the shape the video should be. Pass
/// `None` when no video size is known and it should fill the window again.
///
/// Off Android there is no SurfaceView and the GL path letterboxes itself, so
/// this does nothing.
#[cfg(target_os = "android")]
pub fn set_video_bounds(rect: Option<VideoRect>) {
    android::set_video_bounds(rect);
}

#[cfg(not(target_os = "android"))]
pub fn set_video_bounds(_rect: Option<VideoRect>) {}

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

    /// Hand the activity the rectangle to place its SurfaceView at.
    ///
    /// Failures are logged and swallowed: a video shown at the wrong shape is
    /// worth a line in the log, never a lost playback.
    pub(super) fn set_video_bounds(rect: Option<super::VideoRect>) {
        let activity_ptr = ACTIVITY.load(Ordering::Acquire);
        if activity_ptr.is_null() {
            return;
        }
        let ctx = ndk_context::android_context();
        let Ok(vm) = (unsafe { JavaVM::from_raw(ctx.vm().cast()) }) else {
            return;
        };
        let Ok(mut env) = vm.attach_current_thread() else {
            return;
        };
        let _ = env.exception_clear();

        // A zero size is how the activity is told "not known": it fills the
        // window again, so a shape never outlives the file it came from.
        let (x, y, width, height) = match rect {
            Some(r) => (
                r.x.round() as i32,
                r.y.round() as i32,
                r.width.round() as i32,
                r.height.round() as i32,
            ),
            None => (0, 0, 0, 0),
        };
        let activity = unsafe { JObject::from_raw(activity_ptr.cast()) };
        let result = env.call_method(
            &activity,
            "setVideoBounds",
            "(IIII)V",
            &[x.into(), y.into(), width.into(), height.into()],
        );
        // Always clear, for the same reason `acquire` does: a pending exception
        // would abort the render loop's next JNI call.
        let _ = env.exception_clear();
        match result {
            Err(e) => log::warn!("setVideoBounds({x},{y},{width},{height}) failed: {e}"),
            Ok(_) if width > 0 => {
                log::info!("video surface placed at {width}x{height}+{x}+{y}")
            }
            Ok(_) => log::info!("no video size known; the surface fills the window"),
        }
    }

    /// Drop our reference to the Surface. Only ever called from `acquire`,    /// Drop our reference to the Surface. Only ever called from `acquire`,
    /// which immediately takes a fresh one — see the note above about why
    /// nothing releases on teardown.
    fn release() {
        *HELD.lock().unwrap() = None;
    }
}

#[cfg(test)]
mod tests {
    use super::fit_video;

    /// A 16:9 video on this phone's 2424x1080 display: pillarboxed to 1920 wide
    /// and centred, rather than stretched 26% too wide across the whole thing.
    #[test]
    fn a_wider_container_pillarboxes() {
        let rect = fit_video((2424.0, 1080.0), (1280.0, 720.0)).unwrap();
        assert_eq!((rect.width, rect.height), (1920.0, 1080.0));
        assert_eq!((rect.x, rect.y), (252.0, 0.0));
    }

    /// The same video held portrait: bars above and below instead.
    #[test]
    fn a_taller_container_letterboxes() {
        let rect = fit_video((1080.0, 2424.0), (1280.0, 720.0)).unwrap();
        assert_eq!((rect.width, rect.height), (1080.0, 607.5));
        assert_eq!(rect.x, 0.0);
        assert!((rect.y - 908.25).abs() < 0.01, "{rect:?}");
    }

    #[test]
    fn a_matching_shape_fills_it() {
        let rect = fit_video((1920.0, 1080.0), (1280.0, 720.0)).unwrap();
        assert_eq!(
            (rect.x, rect.y, rect.width, rect.height),
            (0.0, 0.0, 1920.0, 1080.0)
        );
    }

    /// Anamorphic sources are already handled by taking mpv's *display* size,
    /// but the geometry still has to respect a non-square shape it is given.
    #[test]
    fn a_tall_video_is_pillarboxed_hard() {
        let rect = fit_video((2424.0, 1080.0), (1080.0, 1920.0)).unwrap();
        assert_eq!((rect.width, rect.height), (607.5, 1080.0));
    }

    /// Nothing is known yet, or something reported a nonsense size: no rect,
    /// which the caller turns into "fill the window".
    #[test]
    fn a_degenerate_size_has_no_rectangle() {
        assert!(fit_video((2424.0, 1080.0), (0.0, 720.0)).is_none());
        assert!(fit_video((2424.0, 1080.0), (1280.0, 0.0)).is_none());
        assert!(fit_video((0.0, 0.0), (1280.0, 720.0)).is_none());
    }
}
