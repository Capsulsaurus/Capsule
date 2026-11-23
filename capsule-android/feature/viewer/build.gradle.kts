import org.jetbrains.kotlin.gradle.tasks.KotlinCompile

plugins {
    id("capsule.android.feature")
}

android {
    namespace = "com.justin13888.capsule.feature.viewer"
}

// The viewer leans on Material 3 Expressive surfaces the design system wraps but does not
// hide — emphasized typography in particular. Opting in once for the module beats an
// `@OptIn` on every call site: `allWarningsAsErrors` turns a redundant file-level opt-in
// into a build failure, so per-file annotations would need re-auditing every time a
// composable moved between files. This mirrors `core:designsystem` and `core:ui`.
tasks.withType<KotlinCompile>().configureEach {
    compilerOptions {
        optIn.addAll(
            "androidx.compose.material3.ExperimentalMaterial3Api",
            "androidx.compose.material3.ExperimentalMaterial3ExpressiveApi",
            "androidx.compose.foundation.ExperimentalFoundationApi",
        )
    }
}

dependencies {
    // The seeded fixture is the only honest way to reach the degraded states: each one is a
    // named landmark in the generated library rather than a hand-rolled double that would
    // drift from what the repositories actually hand out.
    testImplementation(projects.capsuleAndroid.core.mock)
}
