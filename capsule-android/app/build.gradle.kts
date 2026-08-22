import org.jetbrains.kotlin.gradle.tasks.KotlinCompile

plugins {
    id("capsule.android.application")
    alias(libs.plugins.kotlinxSerialization)
}

android {
    namespace = "com.justin13888.capsule"

    defaultConfig {
        applicationId = "com.justin13888.capsule"
    }

    buildTypes {
        release {
            // Kept off until the app has a signing config and a release pipeline; the
            // only artifact anyone builds today is the debug APK.
            isMinifyEnabled = false
        }
    }
}

// The adaptive shell is built almost entirely out of surfaces that are still opt-in in
// material3 1.5.0-alpha26 and adaptive 1.3.0. Opting in once for the module beats an @OptIn
// on every call site: `allWarningsAsErrors` turns a redundant file-level opt-in into a build
// failure, so per-file annotations would have to be re-audited every time a composable moved
// between files. This mirrors what `core:designsystem` already does.
tasks.withType<KotlinCompile>().configureEach {
    compilerOptions {
        optIn.addAll(
            "androidx.compose.material3.ExperimentalMaterial3Api",
            "androidx.compose.material3.ExperimentalMaterial3ExpressiveApi",
            "androidx.compose.material3.adaptive.ExperimentalMaterial3AdaptiveApi",
        )
    }
}

dependencies {
    implementation(projects.capsuleAndroid.core.model)
    implementation(projects.capsuleAndroid.core.data)
    implementation(projects.capsuleAndroid.core.mock)
    implementation(projects.capsuleAndroid.core.designsystem)
    implementation(projects.capsuleAndroid.core.ui)
    implementation(projects.capsuleAndroid.core.i18n)

    implementation(projects.capsuleAndroid.feature.onboarding)
    implementation(projects.capsuleAndroid.feature.enrollment)
    implementation(projects.capsuleAndroid.feature.timeline)
    implementation(projects.capsuleAndroid.feature.importer)
    implementation(projects.capsuleAndroid.feature.albums)
    implementation(projects.capsuleAndroid.feature.viewer)
    implementation(projects.capsuleAndroid.feature.search)
    implementation(projects.capsuleAndroid.feature.people)
    implementation(projects.capsuleAndroid.feature.sharing)
    implementation(projects.capsuleAndroid.feature.drops)
    implementation(projects.capsuleAndroid.feature.sync)
    implementation(projects.capsuleAndroid.feature.storage)
    implementation(projects.capsuleAndroid.feature.security)
    implementation(projects.capsuleAndroid.feature.recovery)
    implementation(projects.capsuleAndroid.feature.moderation)
    implementation(projects.capsuleAndroid.feature.settings)
    implementation(projects.capsuleAndroid.feature.audit)

    implementation(platform(libs.koin.bom))
    implementation(libs.koin.core)
    implementation(libs.koin.android)
    implementation(libs.koin.androidx.compose)

    implementation(libs.androidx.activity.compose)
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.navigation3.runtime)
    implementation(libs.androidx.navigation3.ui)
    implementation(libs.compose.adaptive)
    implementation(libs.compose.adaptive.layout)
    implementation(libs.compose.adaptive.navigation)
    implementation(libs.compose.material3.navigation.suite)
    implementation(libs.androidx.window)
    implementation(libs.kotlinx.serialization.json)
    implementation(libs.kotlinx.coroutines.core)

    testImplementation(projects.capsuleAndroid.core.testing)
}
