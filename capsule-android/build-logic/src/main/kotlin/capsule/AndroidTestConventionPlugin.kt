package capsule

import com.android.build.api.dsl.CommonExtension
import com.android.build.api.dsl.LibraryExtension
import org.gradle.api.Plugin
import org.gradle.api.Project
import org.gradle.kotlin.dsl.dependencies
import org.gradle.kotlin.dsl.getByType

/**
 * The unit-test harness for Android modules.
 *
 * JUnit 5 is the repo's harness for pure-JVM code. Robolectric, Compose UI test and
 * instrumentation are JUnit 4-only upstream, so the Vintage engine runs those on the
 * same JUnit Platform task — the same one-framework-per-platform carve-out the Clients
 * design doc records for XCTest inside XCUITest bundles on Apple.
 */
class AndroidTestConventionPlugin : Plugin<Project> {
    override fun apply(target: Project) = with(target) {
        // Roborazzi records and verifies screenshots from Robolectric, so screenshot
        // coverage rides the ordinary unit-test task instead of needing a device.
        pluginManager.apply("io.github.takahirom.roborazzi")

        val extension = extensions.getByType<CommonExtension>()
        // Robolectric ships an android-all jar per API level and refuses to run against a
        // targetSdk above the newest it knows (36 in 4.16.x). A library module otherwise
        // inherits compileSdk, which is 37 because material3 1.5 demands it, so its test
        // manifest is pinned down to the level the app actually targets. The application
        // module already declares targetSdk in defaultConfig, and AGP rejects the
        // testOptions form there.
        if (extension is LibraryExtension) {
            extension.testOptions.targetSdk = libs.int("targetSdk")
        }
        extension.testOptions.unitTests.apply {
            // Robolectric and Roborazzi need the merged resources to inflate anything.
            isIncludeAndroidResources = true
            all { it.useJUnitPlatform() }
        }

        dependencies {
            add("testImplementation", platform(libs.lib("junit5-bom")))
            listOf(
                "junit5-jupiter",
                "junit4",
                "robolectric",
                "roborazzi",
                "roborazzi-compose",
                "roborazzi-junit-rule",
                "roborazzi-accessibility-check",
                "turbine",
                "mockk",
                "kotlinx-coroutines-test",
                "compose-ui-test-junit4",
                "androidx-test-junit",
            ).forEach { add("testImplementation", libs.lib(it)) }
            add("testRuntimeOnly", libs.lib("junit5-platform-launcher"))
            add("testRuntimeOnly", libs.lib("junit5-vintage-engine"))
            add("debugImplementation", libs.lib("compose-ui-test-manifest"))
        }
    }
}
