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
}
