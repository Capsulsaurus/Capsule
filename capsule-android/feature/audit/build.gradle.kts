import org.jetbrains.kotlin.gradle.tasks.KotlinCompile

plugins {
    id("capsule.android.feature")
}

android {
    namespace = "com.justin13888.capsule.feature.audit"
}

// The Capsule components this screen is built from wrap Material 3 Expressive surfaces that
// are still opt-in in material3 1.5.0-alpha26, and the opt-in is not `api`. Opting in once
// for the module beats an `@OptIn` per call site: `allWarningsAsErrors` turns a redundant
// file-level annotation into a build failure, so they would need re-auditing every time a
// composable moved file.
tasks.withType<KotlinCompile>().configureEach {
    compilerOptions {
        optIn.addAll(
            "androidx.compose.material3.ExperimentalMaterial3Api",
            "androidx.compose.material3.ExperimentalMaterial3ExpressiveApi",
        )
    }
}

dependencies {
    // The log is derived from the seeded fakes in tests rather than from hand-rolled doubles:
    // a provenance chain that a stub invented would prove the screen renders, not that it
    // renders what the library actually recorded.
    testImplementation(projects.capsuleAndroid.core.mock)
}
