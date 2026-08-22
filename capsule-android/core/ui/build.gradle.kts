import org.jetbrains.kotlin.gradle.tasks.KotlinCompile

plugins {
    id("capsule.android.library")
    id("capsule.android.compose")
    id("capsule.android.test")
}

android {
    namespace = "com.justin13888.capsule.core.ui"
}

// The gallery is built directly on `LazyLayout`, which is the only way to reach a
// non-uniform, plan-driven item space — and the whole of `lazy.layout` is still behind
// `ExperimentalFoundationApi`. Opting in once for the module beats an `@OptIn` on every
// call site: `allWarningsAsErrors` turns a redundant file-level opt-in into a build
// failure, so per-file annotations would need auditing every time code moves file.
tasks.withType<KotlinCompile>().configureEach {
    compilerOptions {
        optIn.addAll(
            "androidx.compose.foundation.ExperimentalFoundationApi",
            "androidx.compose.material3.ExperimentalMaterial3ExpressiveApi",
        )
    }
}

dependencies {
    // The selection set is a few thousand ints on a hot path; `MutableIntSet` keeps that
    // out of the boxing allocator entirely.
    implementation(libs.androidx.collection)

    // Cell chrome, the sticky header and the scrubber bubble are Capsule components, not
    // raw Material ones — the gallery must not invent its own badge or its own selected
    // colour.
    implementation(projects.capsuleAndroid.core.designsystem)

    // Every announced label the gallery owns comes from the catalog.
    implementation(projects.capsuleAndroid.core.i18n)
}
