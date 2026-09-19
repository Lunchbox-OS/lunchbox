//! Hand FFmpeg the process's `JavaVM`.
//!
//! FFmpeg reaches Java from native code through a VM pointer the host has to
//! give it via `av_jni_set_java_vm`. libmpv does not do this — it exports no
//! `JNI_OnLoad` — so unless the app does it, the VM is never registered and
//! every JNI-backed corner of libavcodec fails.
//!
//! The expensive casualty is hardware decoding. FFmpeg's MediaCodec decoders
//! pick a concrete codec by name, and the only enumeration API is
//! `android.media.MediaCodecList`, which is reachable only over JNI. With no
//! VM, `mediacodec-copy` cannot initialise, mpv falls back to decoding on the
//! CPU, and a Fire TV Stick's Cortex-A53 cannot keep up — the ~20fps playback
//! reported in issue #115. (`mediacodec` proper, the zero-copy mode, needs an
//! Android `Surface` and `vo=mediacodec_embed`, which this app's egui
//! compositing does not use; `mediacodec-copy` is the reachable path.)
//!
//! The `audiotrack` AO needs the VM too, which is why libmpv logged
//! "No Java virtual machine has been registered" and fell back to another AO.

/// Register the process's `JavaVM` with FFmpeg. Call once, from
/// `android_main`, before anything constructs a player.
///
/// Best-effort: a failure here costs performance, not correctness, so it is
/// logged rather than propagated.
#[cfg(target_os = "android")]
pub fn register_java_vm() {
    // Declared here rather than bound through a -sys crate: it is one stable C
    // entry point, and `build.rs` already points the linker at the vendored
    // `libavcodec.so` that exports it.
    unsafe extern "C" {
        fn av_jni_set_java_vm(
            vm: *mut core::ffi::c_void,
            log_ctx: *mut core::ffi::c_void,
        ) -> core::ffi::c_int;
    }

    let vm = ndk_context::android_context().vm();
    if vm.is_null() {
        log::error!("no JavaVM in the Android context; MediaCodec decoding will be unavailable");
        return;
    }

    // SAFETY: `vm` is the process-wide `JavaVM*` android-activity published
    // before calling `android_main`, and FFmpeg only stores it.
    let rc = unsafe { av_jni_set_java_vm(vm, core::ptr::null_mut()) };
    if rc < 0 {
        log::error!("av_jni_set_java_vm failed ({rc}); video will decode on the CPU");
    } else {
        log::info!("registered the JavaVM with FFmpeg; MediaCodec decoding is available");
    }
}

/// No-op off Android, where there is no VM to register (the `desktop_preview`
/// example builds this crate for the host).
#[cfg(not(target_os = "android"))]
pub fn register_java_vm() {}
