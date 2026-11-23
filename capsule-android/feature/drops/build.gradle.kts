import org.jetbrains.kotlin.gradle.tasks.KotlinCompile

plugins {
    id("capsule.android.feature")
}

android {
    namespace = "com.justin13888.capsule.feature.drops"
}

// The drop surfaces lean on Material 3 Expressive — the wrapped app bar, the wavy usage
// bars, the caps text fields — and most of that is still behind opt-in markers in
// 1.5.0-alpha26. Opting in once for the module beats an @OptIn per call site:
// `allWarningsAsErrors` turns a redundant file-level opt-in into a build failure, so
// per-file annotations would need re-auditing every time a composable moved between files.
tasks.withType<KotlinCompile>().configureEach {
    compilerOptions {
        optIn.addAll(
            "androidx.compose.material3.ExperimentalMaterial3Api",
            "androidx.compose.material3.ExperimentalMaterial3ExpressiveApi",
        )
    }
}

dependencies {
    // The seeded fakes drive every screen test, so a test asserts against the same library
    // the app runs on rather than against doubles invented for the assertion.
    testImplementation(projects.capsuleAndroid.core.mock)
}
