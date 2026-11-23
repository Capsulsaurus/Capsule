import org.jetbrains.kotlin.gradle.tasks.KotlinCompile

plugins {
    id("capsule.android.feature")
}

android {
    namespace = "com.justin13888.capsule.feature.sync"
}

// The sync centre, the tier queues and the quarantine cards are Material 3
// Expressive surfaces, still behind opt-in markers in the pinned version. Opting in once
// for the module beats an `@OptIn` on every call site: `allWarningsAsErrors` turns a
// redundant file-level opt-in into a build failure, so per-file annotations would need
// re-auditing every time a composable moved between files.
tasks.withType<KotlinCompile>().configureEach {
    compilerOptions {
        optIn.addAll(
            "androidx.compose.material3.ExperimentalMaterial3Api",
            "androidx.compose.material3.ExperimentalMaterial3ExpressiveApi",
        )
    }
}

dependencies {
    // Previews and tests drive the seeded fakes; production code names only `core:data`.
    testImplementation(projects.capsuleAndroid.core.mock)
}
