import org.gradle.api.tasks.Exec

plugins {
    id("com.android.application")
}

// Add "x86_64" here to also target the emulator. Drives both the packaged ABIs
// and the cargo-ndk cross-compile below. armeabi-v7a covers 32-bit-only Fire TV
// sticks (e.g. AFTHA004); arm64-v8a covers modern phones and 64-bit TVs.
val rustAbis = listOf("arm64-v8a", "armeabi-v7a")

// Single source of truth: the repo-root VERSION file, three directories up from
// this Gradle project (android/ -> lunchbox-media-android/ -> crates/ -> root).
// Mirrors companion-android and scripts/lib/version.sh so versionName can't drift.
val shepherdVersion: String =
    rootProject.projectDir.parentFile.parentFile.parentFile.resolve("VERSION")
        .readText().trim()

// Monotonic versionCode packed from semver (minor/patch < 100). e.g. 0.2.0 -> 200.
val shepherdVersionCode: Int =
    shepherdVersion.substringBefore('-').substringBefore('+').split('.').let {
        it[0].toInt() * 10000 + it[1].toInt() * 100 + it[2].toInt()
    }

android {
    namespace = "com.armeafamily.shepherd.media"
    compileSdk = 35

    defaultConfig {
        applicationId = "com.armeafamily.shepherd.media"
        minSdk = 24
        targetSdk = 35
        versionCode = shepherdVersionCode
        versionName = shepherdVersion
        ndk {
            //noinspection ChromeOsAbiSupport
            abiFilters += rustAbis
        }
    }

    // Release signing is configured only when the CI keystore env vars are
    // present (see .github/workflows/release.yml). Local `assembleRelease`
    // without them falls back to debug signing, so developers can still produce
    // an installable APK without the release key.
    val releaseKeystore: String? = System.getenv("SHEPHERD_KEYSTORE_FILE")
    signingConfigs {
        if (releaseKeystore != null) {
            create("release") {
                storeFile = file(releaseKeystore)
                storePassword = System.getenv("SHEPHERD_KEYSTORE_PASSWORD")
                keyAlias = System.getenv("SHEPHERD_KEY_ALIAS")
                keyPassword = System.getenv("SHEPHERD_KEY_PASSWORD")
            }
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            signingConfig = signingConfigs.findByName("release")
                ?: signingConfigs.getByName("debug")
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
    // Rust cdylib (liblunchbox_media_android.so) built by cargo-ndk below.
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
        "build", "-p", "lunchbox-media-android", "--release",
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
