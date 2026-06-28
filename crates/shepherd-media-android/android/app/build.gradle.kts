import org.gradle.api.tasks.Exec

plugins {
    id("com.android.application")
}

// Single ABI for now; add "x86_64" here to also target the emulator. Drives
// both the packaged ABIs and the cargo-ndk cross-compile below.
val rustAbis = listOf("arm64-v8a")

android {
    namespace = "com.armeafamily.shepherdmedia"
    compileSdk = 35

    defaultConfig {
        applicationId = "com.armeafamily.shepherdmedia"
        minSdk = 24
        targetSdk = 35
        versionCode = 1
        versionName = "0.1.0"
        ndk {
            //noinspection ChromeOsAbiSupport
            abiFilters += rustAbis
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
        }
    }

    // youtubedl-android extracts its bundled Python from the APK at runtime, so
    // the native libs must be page-aligned-uncompressed on disk.
    packaging {
        jniLibs {
            useLegacyPackaging = true
        }
    }

    // No Java/Kotlin sources: the app is a pure NativeActivity that loads the
    // Rust cdylib (libshepherd_media_android.so) built by cargo-ndk below.
    // jniLibs come from two places: our cargo-ndk output, and the vendored
    // libmpv + ffmpeg shared libraries the Rust cdylib links against.
    sourceSets {
        getByName("main") {
            jniLibs.srcDir(layout.buildDirectory.dir("rustJniLibs"))
            jniLibs.srcDir(rootProject.projectDir.parentFile.resolve("vendor/libmpv"))
        }
    }
}

// Cross-compile the Rust cdylib into the jniLibs dir before packaging.
//
// Requires `cargo-ndk` (cargo install cargo-ndk) and ANDROID_NDK_HOME pointing
// at an installed NDK. The workingDir walks up from android/app to the repo root
// so cargo resolves the workspace.
val cargoNdkBuild by tasks.registering(Exec::class) {
    group = "rust"
    description = "Build the Rust cdylib for Android via cargo-ndk"
    workingDir = rootProject.projectDir.parentFile.parentFile.parentFile

    val outDir = layout.buildDirectory.dir("rustJniLibs").get().asFile
    outDir.mkdirs()

    val args = mutableListOf("cargo", "ndk")
    rustAbis.forEach { abi ->
        args += listOf("-t", abi)
    }
    args += listOf(
        "-o", outDir.absolutePath,
        "build", "-p", "shepherd-media-android", "--release",
    )
    commandLine(args)
}

tasks.named("preBuild") {
    dependsOn(cargoNdkBuild)
}

dependencies {
    // yt-dlp bundled with a Python runtime, called over JNI from the Rust code
    // (see src/youtube.rs). Brings its own native libs/assets, packaged into
    // the APK automatically.
    implementation("io.github.junkfood02.youtubedl-android:library:0.18.1")
}
