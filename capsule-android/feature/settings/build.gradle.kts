import org.jetbrains.kotlin.gradle.tasks.KotlinCompile

plugins {
    id("capsule.android.feature")
}

android {
    namespace = "com.justin13888.capsule.feature.settings"
}

// The Capsule components these pages are built from wrap Material 3 Expressive surfaces that
// are still opt-in in material3 1.5.0-alpha26, and the opt-in is not `api`. Opting in once for
// the module beats an `@OptIn` per call site: `allWarningsAsErrors` turns a redundant
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
    // The appearance and language preferences belong to this device rather than to the
    // library, so no `core:data` contract owns them. DataStore rather than SharedPreferences
    // for the one property this screen depends on: a write completes before the suspending
    // call returns, so a crash straight after a toggle cannot silently lose it.
    implementation(libs.androidx.datastore.preferences)

    // The settings pages read six repositories at once, and the assertions worth making are
    // about how those readings combine. Hand-rolled doubles would prove the combine compiles.
    testImplementation(projects.capsuleAndroid.core.mock)
}
