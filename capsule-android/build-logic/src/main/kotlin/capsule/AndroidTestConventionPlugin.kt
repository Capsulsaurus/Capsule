package capsule

import com.android.build.api.dsl.CommonExtension
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
        val extension = extensions.getByType<CommonExtension>()
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
