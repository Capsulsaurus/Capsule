package capsule

import org.gradle.api.Plugin
import org.gradle.api.Project
import org.gradle.kotlin.dsl.dependencies
import org.gradle.kotlin.dsl.project

/**
 * A feature module: an Android library with Compose, the shared core modules on its
 * classpath, and the standard test harness.
 *
 * A feature owns the screens and view models for one product area. It depends on core,
 * never on another feature — the app module is the only place features meet.
 */
class AndroidFeatureConventionPlugin : Plugin<Project> {
    override fun apply(target: Project) = with(target) {
        pluginManager.apply("capsule.android.library")
        pluginManager.apply("capsule.android.compose")
        pluginManager.apply("capsule.android.test")

        dependencies {
            listOf(
                ":capsule-android:core:model",
                ":capsule-android:core:data",
                ":capsule-android:core:designsystem",
                ":capsule-android:core:ui",
                ":capsule-android:core:i18n",
            ).forEach { add("implementation", project(it)) }
            add("testImplementation", project(":capsule-android:core:testing"))

            add("implementation", platform(libs.lib("koin-bom")))
            listOf(
                "koin-core",
                "koin-android",
                "koin-androidx-compose",
                "kotlinx-coroutines-core",
            ).forEach { add("implementation", libs.lib(it)) }
        }
    }
}
