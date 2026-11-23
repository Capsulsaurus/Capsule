import org.jetbrains.kotlin.gradle.tasks.KotlinCompile

plugins {
    id("capsule.android.feature")
}

android {
    namespace = "com.justin13888.capsule.feature.search"
}

// The sheets, bars and emphasized type this screen set is built from are still opt-in in
// material3 1.5.0-alpha26, and a default argument on a design-system wrapper is evaluated
// at *this* module's call site — so opting in here is not optional even though the module
// touches no Material component directly. Once for the module rather than per file:
// `allWarningsAsErrors` turns a redundant file-level opt-in into a build failure, so
// annotations would have to be re-audited every time a composable moved between files.
tasks.withType<KotlinCompile>().configureEach {
    compilerOptions {
        optIn.addAll(
            "androidx.compose.material3.ExperimentalMaterial3Api",
            "androidx.compose.material3.ExperimentalMaterial3ExpressiveApi",
        )
    }
}

dependencies {
    // The fakes drive every preview-shaped test in this module; production code depends on
    // the `core:data` contracts only.
    testImplementation(projects.capsuleAndroid.core.mock)
}
