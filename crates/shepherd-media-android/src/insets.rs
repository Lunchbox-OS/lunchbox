//! Display safe-area insets: how far in from each window edge the app must keep
//! its content so nothing sits under a camera cutout or is clipped by the
//! display's rounded corners.
//!
//! The window renders edge-to-edge (see the `shortEdges` cutout mode in
//! `res/values/themes.xml`); the UI then insets its content by these values so
//! the background fills the whole non-rectangular display while posters, text,
//! and controls stay inside the safe rectangle.
//!
//! On Android the values come from the decor view's `WindowInsets` over JNI
//! (display-cutout safe insets, widened per edge by the adjacent rounded-corner
//! radii). Everything degrades to zero insets on any error or on the host build,
//! so the desktop preview and older devices simply render as before.

/// Safe-area insets in physical pixels, one per window edge.
#[derive(Clone, Copy, Default, Debug)]
pub struct SafeInsets {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

/// Query the current safe-area insets. Returns all-zero if they can't be
/// determined (host build, view not yet attached, older API, JNI error).
#[cfg(target_os = "android")]
pub fn query() -> SafeInsets {
    android::query().unwrap_or_default()
}

#[cfg(not(target_os = "android"))]
pub fn query() -> SafeInsets {
    SafeInsets::default()
}

/// Record the `NativeActivity` instance (`AndroidApp::activity_as_ptr()`) so the
/// insets query can reach `getWindow()`. Must be called from `android_main`
/// before the UI runs. `ndk_context`'s context is the `Application`, which has
/// no window, so we need the activity handle explicitly.
#[cfg(target_os = "android")]
pub fn set_activity(activity: *mut core::ffi::c_void) {
    android::ACTIVITY.store(activity, std::sync::atomic::Ordering::Release);
}

#[cfg(not(target_os = "android"))]
pub fn set_activity(_activity: *mut core::ffi::c_void) {}

#[cfg(target_os = "android")]
mod android {
    use super::SafeInsets;
    use core::ffi::c_void;
    use jni::JavaVM;
    use jni::objects::{JObject, JValue};
    use std::sync::atomic::{AtomicPtr, Ordering};

    /// The `NativeActivity` instance jobject, set once from `android_main`.
    pub(super) static ACTIVITY: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

    pub fn query() -> Option<SafeInsets> {
        let activity_ptr = ACTIVITY.load(Ordering::Acquire);
        if activity_ptr.is_null() {
            return None;
        }
        let ctx = ndk_context::android_context();
        let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }.ok()?;
        let mut env = vm.attach_current_thread().ok()?;
        // Don't inherit a Java exception left pending by earlier JNI work.
        let _ = env.exception_clear();

        // Do the work, then always clear any exception we might have raised so a
        // failure here can never abort the render loop's next JNI call.
        let result = query_insets(&mut env, activity_ptr);
        let _ = env.exception_clear();
        result
    }

    fn query_insets(env: &mut jni::JNIEnv, activity_ptr: *mut c_void) -> Option<SafeInsets> {
        let activity = unsafe { JObject::from_raw(activity_ptr.cast()) };
        let window = env
            .call_method(&activity, "getWindow", "()Landroid/view/Window;", &[])
            .ok()?
            .l()
            .ok()?;
        let decor = env
            .call_method(&window, "getDecorView", "()Landroid/view/View;", &[])
            .ok()?
            .l()
            .ok()?;
        let insets = env
            .call_method(
                &decor,
                "getRootWindowInsets",
                "()Landroid/view/WindowInsets;",
                &[],
            )
            .ok()?
            .l()
            .ok()?;
        if insets.is_null() {
            // The view isn't attached yet; a later refresh will pick it up.
            return None;
        }

        // Display-cutout safe insets (API 30+): getInsets(Type.displayCutout()).
        let type_mask = env
            .call_static_method(
                "android/view/WindowInsets$Type",
                "displayCutout",
                "()I",
                &[],
            )
            .ok()?
            .i()
            .ok()?;
        let cutout = env
            .call_method(
                &insets,
                "getInsets",
                "(I)Landroid/graphics/Insets;",
                &[JValue::Int(type_mask)],
            )
            .ok()?
            .l()
            .ok()?;
        let mut left = int_field(env, &cutout, "left");
        let mut top = int_field(env, &cutout, "top");
        let mut right = int_field(env, &cutout, "right");
        let mut bottom = int_field(env, &cutout, "bottom");

        // Rounded corners (API 31+): keep content a corner-radius clear of each
        // corner so it isn't clipped by the display's rounded edges. A rectangle
        // inset by R fits inside a corner of radius R.
        let [tl, tr, br, bl] = rounded_corner_radii(env, &insets);
        left = left.max(tl).max(bl);
        right = right.max(tr).max(br);
        top = top.max(tl).max(tr);
        bottom = bottom.max(bl).max(br);

        Some(SafeInsets {
            left: left as f32,
            top: top as f32,
            right: right as f32,
            bottom: bottom as f32,
        })
    }

    fn int_field(env: &mut jni::JNIEnv, obj: &JObject, name: &str) -> i32 {
        env.get_field(obj, name, "I")
            .and_then(|v| v.i())
            .unwrap_or(0)
    }

    /// Radii of the four rounded corners, in `[top-left, top-right,
    /// bottom-right, bottom-left]` order (the `RoundedCorner.POSITION_*`
    /// constants 0..=3). Zero for any corner that is square, unavailable, or on
    /// an API level without `getRoundedCorner` (< 31).
    fn rounded_corner_radii(env: &mut jni::JNIEnv, insets: &JObject) -> [i32; 4] {
        let mut radii = [0i32; 4];
        for (slot, position) in radii.iter_mut().zip(0..4) {
            let corner = env.call_method(
                insets,
                "getRoundedCorner",
                "(I)Landroid/view/RoundedCorner;",
                &[JValue::Int(position)],
            );
            // getRoundedCorner is missing before API 31; swallow the exception.
            if env.exception_check().unwrap_or(false) {
                let _ = env.exception_clear();
                continue;
            }
            let Some(corner) = corner.ok().and_then(|v| v.l().ok()) else {
                continue;
            };
            if corner.is_null() {
                continue;
            }
            *slot = env
                .call_method(&corner, "getRadius", "()I", &[])
                .ok()
                .and_then(|v| v.i().ok())
                .unwrap_or(0);
        }
        radii
    }
}
