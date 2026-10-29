import org.jetbrains.kotlin.gradle.dsl.JvmTarget

// The responsiveness parity benchmark (Clients - Responsiveness): a Macrobenchmark test APK
// that drives `:android`'s `benchmark` build and the platform photo app on one physical device.
// Run it with `mise run bench-apps android <device>`; it needs a device, so no CI gate on a pull
// request builds or runs it beyond compilation.
//
// The plugins come without versions: AGP and the Kotlin Gradle plugin are already on the build
// classpath through the root project's `apply false` aliases.
plugins {
    id("com.android.test")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "com.justin13888.capsule.benchmark"
    compileSdk = 36

    defaultConfig {
        // FrameTimingMetric reads frame timelines, which the platform only exposes from API 31.
        minSdk = 31
        targetSdk = 36
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }

    buildTypes {
        // Matches `:android`'s `benchmark` type. Debuggable here is the test APK only; the app
        // it measures stays non-debuggable.
        create("benchmark") {
            isDebuggable = true
            signingConfig = signingConfigs.getByName("debug")
            matchingFallbacks += listOf("release")
        }
    }

    targetProjectPath = ":android"
    experimentalProperties["android.experimental.self-instrumenting"] = true

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

kotlin {
    compilerOptions {
        jvmTarget.set(JvmTarget.JVM_17)
    }
}

// Only the `benchmark` variant measures anything meaningful; a debug variant would time a
// debuggable app.
androidComponents {
    beforeVariants(selector().all()) { variant ->
        variant.enable = variant.buildType == "benchmark"
    }
}

dependencies {
    implementation(libs.androidx.benchmark.macro.junit4)
    implementation(libs.androidx.test.ext.junit)
    implementation(libs.androidx.test.uiautomator)
    implementation(libs.androidx.tracing.ktx)
}
