import org.jetbrains.kotlin.gradle.dsl.JvmTarget

// A plain Android application: AGP, the Kotlin Android plugin and the Compose compiler, with
// every androidx.compose artifact versioned by the Jetpack Compose BOM. Clients are native
// (ADR-0013), so no Kotlin or Compose Multiplatform plugin is applied, and the sources sit in
// the standard `src/main` and `src/test` sets that ktlint and detekt read by default.
plugins {
    alias(libs.plugins.androidApplication)
    alias(libs.plugins.kotlinAndroid)
    alias(libs.plugins.composeCompiler)
    alias(libs.plugins.kotlinxSerialization)
}

kotlin {
    compilerOptions {
        jvmTarget.set(JvmTarget.JVM_17)
    }
}

android {
    namespace = "com.justin13888.capsule"
    compileSdk = 36

    defaultConfig {
        applicationId = "com.justin13888.capsule"
        minSdk = 26
        targetSdk = 36
        // Version is the repo-wide source of truth in gradle.properties, kept in sync
        // across every package by `mise run set-version` (xtask).
        versionCode = providers.gradleProperty("capsule.versionCode").get().toInt()
        versionName = providers.gradleProperty("capsule.versionName").get()
    }
    buildFeatures {
        compose = true
    }
    packaging {
        resources {
            excludes += "/META-INF/{AL2.0,LGPL2.1}"
        }
    }
    buildTypes {
        getByName("release") {
            isMinifyEnabled = false
        }
        // What the parity benchmark (`:benchmark`) measures: release code, never debuggable
        // (a debuggable build runs interpreted and would time the wrong app), signed with the
        // debug key so a bench device installs it without a release keystore.
        create("benchmark") {
            initWith(getByName("release"))
            signingConfig = signingConfigs.getByName("debug")
            matchingFallbacks += listOf("release")
            isDebuggable = false
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

dependencies {
    implementation(platform(libs.androidx.compose.bom))
    implementation(libs.androidx.compose.runtime)
    implementation(libs.androidx.compose.foundation)
    implementation(libs.androidx.compose.ui)
    implementation(libs.androidx.compose.material3)
    implementation(libs.androidx.compose.material.icons.core)
    implementation(libs.androidx.compose.ui.tooling.preview)
    debugImplementation(libs.androidx.compose.ui.tooling)

    implementation(libs.androidx.activity.compose)
    // The in-app language picker below Android 13 (AppCompatDelegate.setApplicationLocales).
    implementation(libs.androidx.appcompat)
    implementation(libs.androidx.viewmodel.compose)
    implementation(libs.androidx.navigation.compose)
    implementation(libs.koin.androidx.compose)
    implementation(libs.coil.compose)
    implementation(libs.coil.network.ktor)
    // JankStats (field frame telemetry) and the trace sections the parity benchmark reads.
    implementation(libs.androidx.metrics.performance)
    implementation(libs.androidx.tracing.ktx)
    implementation(libs.kotlinx.serialization.core)
    implementation(projects.core)

    // Plain JVM unit tests over the alert loop, which touches no Android API.
    testImplementation(kotlin("test"))
}

// The adapters' JVM unit tests drive a real FfiWorkspace: JNA loads the host libcapsule_sdk
// that `-Pcapsule.wireFfi` builds into the repo's target/debug, as `:core`'s smoke does.
tasks.withType<Test>().configureEach {
    systemProperty("jna.library.path", "${projectDir.parentFile}/target/debug")
}
