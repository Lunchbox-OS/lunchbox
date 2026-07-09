//! Local shared-storage access for the in-app file browser.
//!
//! The Add-library form lets the user browse to a `.toml`/`.m3u` on the device
//! instead of typing a path — the practical way to add an **offline** library on
//! a TV with no keyboard. Because the browser hands the resolver a real
//! filesystem path (not a SAF `content://`), a library file's relative media and
//! posters resolve against its own directory exactly as on Linux; that is why
//! this is a plain-path browser rather than the SAF document picker (which grants
//! access to a single opaque document and cannot reach its siblings).
//!
//! Reading arbitrary files under shared storage needs the "All files access"
//! special permission on API 30+ (`MANAGE_EXTERNAL_STORAGE`). We can't grant it
//! ourselves — [`request_all_files_access`] opens the system settings screen and
//! the user toggles it; [`has_all_files_access`] re-checks on return. This is the
//! only capability the browser needs, so the app stays a pure `NativeActivity`
//! with no Java/Kotlin. It reaches internal shared storage and SD cards, but not
//! USB-OTG drives (those are SAF-only).
//!
//! Everything degrades gracefully on the host build (no JNI): the browser roots
//! at `$HOME` and access is always "granted", so the desktop preview can drive
//! the same UI.

use std::path::PathBuf;

/// The directory the browser can navigate up to but not past.
#[cfg(target_os = "android")]
pub const FLOOR: &str = "/storage";
#[cfg(not(target_os = "android"))]
pub const FLOOR: &str = "/";

/// The directory the browser opens at: the primary shared-storage root
/// (`/storage/emulated/0`), or `$HOME` on the host. Falls back to [`FLOOR`].
pub fn browse_root() -> PathBuf {
    #[cfg(target_os = "android")]
    {
        android::external_root().unwrap_or_else(|| PathBuf::from("/storage/emulated/0"))
    }
    #[cfg(not(target_os = "android"))]
    {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(FLOOR))
    }
}

/// Whether the app may read arbitrary files under shared storage. Always true
/// before API 30 (legacy storage) and on the host.
pub fn has_all_files_access() -> bool {
    #[cfg(target_os = "android")]
    {
        android::is_external_storage_manager().unwrap_or(true)
    }
    #[cfg(not(target_os = "android"))]
    {
        true
    }
}

/// Open the system "All files access" settings screen for this app. The user
/// grants the permission there; we re-check with [`has_all_files_access`] when
/// the browser is next opened. No-op on the host.
pub fn request_all_files_access() {
    #[cfg(target_os = "android")]
    {
        android::request_all_files_access();
    }
}

/// Record the `NativeActivity` instance so storage JNI calls can reach a
/// `Context` (for `getPackageName`/`startActivity`). Call from `android_main`.
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
    use jni::objects::{JObject, JString, JValue};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicPtr, Ordering};

    /// The `NativeActivity` instance jobject, set once from `android_main`.
    pub(super) static ACTIVITY: AtomicPtr<c_void> = AtomicPtr::new(std::ptr::null_mut());

    /// Attach to the JVM and run `f` with the JNI env, clearing any pending Java
    /// exception before and after so a failure can never poison a later call.
    fn with_env<T>(f: impl FnOnce(&mut jni::JNIEnv) -> Option<T>) -> Option<T> {
        let ctx = ndk_context::android_context();
        let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }.ok()?;
        let mut env = vm.attach_current_thread().ok()?;
        let _ = env.exception_clear();
        let out = f(&mut env);
        let _ = env.exception_clear();
        out
    }

    fn activity() -> Option<JObject<'static>> {
        let ptr = ACTIVITY.load(Ordering::Acquire);
        if ptr.is_null() {
            None
        } else {
            Some(unsafe { JObject::from_raw(ptr.cast()) })
        }
    }

    pub fn external_root() -> Option<PathBuf> {
        with_env(|env| {
            let file = env
                .call_static_method(
                    "android/os/Environment",
                    "getExternalStorageDirectory",
                    "()Ljava/io/File;",
                    &[],
                )
                .ok()?
                .l()
                .ok()?;
            let path = env
                .call_method(&file, "getAbsolutePath", "()Ljava/lang/String;", &[])
                .ok()?
                .l()
                .ok()?;
            let s: String = env.get_string(&JString::from(path)).ok()?.into();
            Some(PathBuf::from(s))
        })
    }

    pub fn is_external_storage_manager() -> Option<bool> {
        with_env(|env| {
            let r = env.call_static_method(
                "android/os/Environment",
                "isExternalStorageManager",
                "()Z",
                &[],
            );
            // Missing before API 30 — treat as "legacy access, assume granted".
            if env.exception_check().unwrap_or(false) {
                let _ = env.exception_clear();
                return Some(true);
            }
            r.ok()?.z().ok()
        })
    }

    pub fn request_all_files_access() {
        with_env(|env| {
            let activity = activity()?;
            let pkg_obj = env
                .call_method(&activity, "getPackageName", "()Ljava/lang/String;", &[])
                .ok()?
                .l()
                .ok()?;
            let pkg: String = env.get_string(&JString::from(pkg_obj)).ok()?.into();

            let action = env
                .new_string("android.settings.MANAGE_APP_ALL_FILES_ACCESS_PERMISSION")
                .ok()?;
            let intent = env
                .new_object(
                    "android/content/Intent",
                    "(Ljava/lang/String;)V",
                    &[JValue::Object(&action)],
                )
                .ok()?;
            let data = env.new_string(format!("package:{pkg}")).ok()?;
            let uri = env
                .call_static_method(
                    "android/net/Uri",
                    "parse",
                    "(Ljava/lang/String;)Landroid/net/Uri;",
                    &[JValue::Object(&data)],
                )
                .ok()?
                .l()
                .ok()?;
            env.call_method(
                &intent,
                "setData",
                "(Landroid/net/Uri;)Landroid/content/Intent;",
                &[JValue::Object(&uri)],
            )
            .ok()?;
            env.call_method(
                &activity,
                "startActivity",
                "(Landroid/content/Intent;)V",
                &[JValue::Object(&intent)],
            )
            .ok()?;
            Some(())
        });
    }
}
