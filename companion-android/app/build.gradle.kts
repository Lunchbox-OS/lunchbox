plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.android)
    alias(libs.plugins.kotlin.compose)
    alias(libs.plugins.kotlin.serialization)
}

// Single source of truth: the repo-root VERSION file (one directory above this
// Gradle project). Read it at configure time so `versionName` can never drift
// from the rest of the monorepo. See scripts/lib/version.sh.
val shepherdVersion: String =
    rootProject.projectDir.parentFile.resolve("VERSION").readText().trim()

// Derive a monotonic versionCode from the canonical semver so in-place upgrades
// work without a second source of truth. Strip any -prerelease/+build suffix,
// then pack major.minor.patch as MMMMMNNPP (minor/patch < 100). e.g. 0.2.0 -> 200.
val shepherdVersionCode: Int =
    shepherdVersion.substringBefore('-').substringBefore('+').split('.').let {
        it[0].toInt() * 10000 + it[1].toInt() * 100 + it[2].toInt()
    }

android {
    namespace = "com.armeafamily.shepherd.companion"
    compileSdk = 35

    defaultConfig {
        applicationId = "com.armeafamily.shepherd.companion"
        minSdk = 31
        targetSdk = 35
        versionCode = shepherdVersionCode
        versionName = shepherdVersion

        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }

    // Release signing is configured only when the CI keystore env vars are
    // present (see .github/workflows/release.yml). Local `assembleRelease`
    // without them falls back to the debug signing config below, so developers
    // can still produce an installable APK without the release key.
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
            // No telemetry, no analytics — nothing to strip beyond unused
            // code. Minify is off so sideloaded debug builds match release
            // behaviour byte-for-byte during the project's early life.
            isMinifyEnabled = false
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro",
            )
            signingConfig = signingConfigs.findByName("release")
                ?: signingConfigs.getByName("debug")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlinOptions {
        jvmTarget = "17"
    }

    buildFeatures {
        compose = true
    }

    testOptions {
        unitTests.all {
            it.useJUnitPlatform()
        }
    }

    packaging {
        resources {
            excludes += "/META-INF/{AL2.0,LGPL2.1}"
        }
    }
}

dependencies {
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.lifecycle.runtime.compose)
    implementation(libs.androidx.lifecycle.viewmodel.compose)
    implementation(libs.androidx.activity.compose)
    implementation(libs.androidx.navigation.compose)

    implementation(platform(libs.androidx.compose.bom))
    implementation(libs.androidx.compose.ui)
    implementation(libs.androidx.compose.ui.graphics)
    implementation(libs.androidx.compose.ui.tooling.preview)
    implementation(libs.androidx.compose.material3)
    implementation(libs.androidx.compose.material.icons.extended)
    debugImplementation(libs.androidx.compose.ui.tooling)

    implementation(libs.accompanist.permissions)

    implementation(libs.kotlinx.coroutines.android)
    implementation(libs.kotlinx.serialization.json)
    implementation(libs.kable.core)

    implementation(libs.androidx.security.crypto)
    implementation(libs.androidx.datastore.preferences)

    testImplementation(libs.junit.jupiter)
    testImplementation(libs.mockk)
    testImplementation(libs.turbine)
    testImplementation(libs.kotlinx.coroutines.test)
}
