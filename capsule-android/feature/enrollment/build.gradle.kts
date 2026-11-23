import org.jetbrains.kotlin.gradle.tasks.KotlinCompile

plugins {
    id("capsule.android.feature")
}

android {
    namespace = "com.justin13888.capsule.feature.enrollment"
}

// The enrollment surfaces lean on the same still-opt-in Material 3 Expressive APIs the design
// system wraps. Opting in once for the module beats an annotation per call site:
// `allWarningsAsErrors` turns a redundant file-level opt-in into a build failure, so per-file
// markers would need re-auditing every time a composable moved between files.
tasks.withType<KotlinCompile>().configureEach {
    compilerOptions {
        optIn.addAll(
            "androidx.compose.material3.ExperimentalMaterial3Api",
            "androidx.compose.material3.ExperimentalMaterial3ExpressiveApi",
            // `Dispatchers.setMain` and `advanceUntilIdle` are the whole of the test harness
            // for a view model and are still marked experimental upstream.
            "kotlinx.coroutines.ExperimentalCoroutinesApi",
        )
    }
}

dependencies {
    // `EnrollmentGateway` lives in `core:mock` because no `core:data` contract owns adding a
    // device yet, and its own documentation says a feature written against it is written
    // against a mock type on purpose — so that this dependency breaks loudly the day the real
    // contract lands. Depending on it here rather than inventing a parallel fake keeps the one
    // seeded backend authoritative for what the device directory contains.
    implementation(projects.capsuleAndroid.core.mock)

    // A real camera preview for the scanner. The decode over it is mocked; no barcode library
    // is pulled in for a screen whose input is a fixture.
    implementation(libs.androidx.camera.camera2)
    implementation(libs.androidx.camera.lifecycle)
    implementation(libs.androidx.camera.compose)
    implementation(libs.androidx.activity.compose)
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.lifecycle.runtime.compose)
}
