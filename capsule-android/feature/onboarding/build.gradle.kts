import org.jetbrains.kotlin.gradle.tasks.KotlinCompile

plugins {
    id("capsule.android.feature")
}

android {
    namespace = "com.justin13888.capsule.feature.onboarding"
}

// The onboarding surfaces lean on the same still-opt-in Material 3 Expressive APIs the
// design system wraps — bottom sheets, and the expressive typography scale. Opting in once
// for the module beats an annotation per call site: `allWarningsAsErrors` turns a redundant
// file-level opt-in into a build failure, so per-file markers would need re-auditing every
// time a composable moved between files.
tasks.withType<KotlinCompile>().configureEach {
    compilerOptions {
        optIn.addAll(
            "androidx.compose.material3.ExperimentalMaterial3Api",
            "androidx.compose.material3.ExperimentalMaterial3ExpressiveApi",
            // `Dispatchers.setMain` and `advanceUntilIdle` are the whole of the test
            // harness for a view model and are still marked experimental upstream.
            "kotlinx.coroutines.ExperimentalCoroutinesApi",
        )
    }
}

dependencies {
    // The media-permission primer asks the platform what this app may currently read, and
    // requests more through an activity-result launcher.
    implementation(libs.androidx.activity.compose)
    implementation(libs.androidx.core.ktx)

    testImplementation(projects.capsuleAndroid.core.mock)
}
