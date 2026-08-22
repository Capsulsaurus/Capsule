import org.jetbrains.kotlin.gradle.tasks.KotlinCompile

plugins {
    id("capsule.android.library")
    id("capsule.android.compose")
    id("capsule.android.test")
}

android {
    namespace = "com.justin13888.capsule.core.designsystem"
}

// Material 3 Expressive is the whole point of this module, and in material3
// 1.5.0-alpha26 most of that surface still sits behind opt-in markers. Opting in once
// for the module beats an @OptIn on every call site: `allWarningsAsErrors` turns a
// redundant file-level opt-in into a build failure, so per-file annotations would have
// to be audited by hand every time a component moves between files.
tasks.withType<KotlinCompile>().configureEach {
    compilerOptions {
        optIn.addAll(
            "androidx.compose.material3.ExperimentalMaterial3Api",
            "androidx.compose.material3.ExperimentalMaterial3ExpressiveApi",
        )
    }
}

dependencies {
    // `api`, not `implementation`: both show up in this module's public signatures —
    // icons are handed to the component wrappers by callers, and the shape morphs
    // expose `RoundedPolygon` so features can pick their own start/end shapes.
    api(libs.compose.material.icons.extended)
    api(libs.androidx.graphics.shapes)

    // Previews render real catalog strings rather than invented copy, so the gallery
    // stays honest about what a localized screen looks like.
    implementation(projects.capsuleAndroid.core.i18n)
}
