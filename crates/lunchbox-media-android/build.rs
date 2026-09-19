//! Point the linker at the vendored Android libmpv so the `-lmpv` that
//! `libmpv2-sys` emits resolves. libmpv's own dependencies (libav*, libsw*,
//! libc++_shared) are resolved at runtime from the APK's `jniLibs`, so they are
//! not required at link time.
//!
//! On non-Android hosts this does nothing: the system libmpv (from
//! `libmpv-dev`, the same one `lunchbox-media` links against) satisfies `-lmpv`.

use std::env;
use std::path::PathBuf;

fn main() {
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "android" {
        return;
    }

    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let abi = match target_arch.as_str() {
        "aarch64" => "arm64-v8a",
        "arm" => "armeabi-v7a",
        other => panic!("no vendored libmpv for Android target arch `{other}`"),
    };

    let dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("vendor/libmpv")
        .join(abi);
    println!("cargo:rustc-link-search=native={}", dir.display());
    // `av_jni_set_java_vm` (src/ffmpeg.rs) is called directly, so libavcodec has
    // to be a link-time dependency as well as a runtime one. Its SONAME is a
    // plain `libavcodec.so`, which is also the name it is packaged under in the
    // APK's jniLibs, so the runtime lookup resolves to the same file.
    println!("cargo:rustc-link-lib=dylib=avcodec");
    println!("cargo:rustc-link-arg=-Wl,--allow-shlib-undefined");
}
