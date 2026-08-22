package capsule

import org.gradle.api.JavaVersion
import org.gradle.api.Plugin
import org.gradle.api.Project
import org.gradle.api.plugins.JavaPluginExtension
import org.gradle.api.tasks.testing.Test
import org.gradle.kotlin.dsl.configure
import org.gradle.kotlin.dsl.dependencies
import org.gradle.kotlin.dsl.withType
import org.jetbrains.kotlin.gradle.dsl.JvmTarget
import org.jetbrains.kotlin.gradle.tasks.KotlinCompile

/**
 * A pure-JVM Kotlin module — no Android framework on the classpath, so its tests run
 * without Robolectric and stay fast. Used for the domain model and the layout and
 * planning logic, none of which has any reason to know about Android.
 */
class JvmLibraryConventionPlugin : Plugin<Project> {
    override fun apply(target: Project) = with(target) {
        pluginManager.apply("org.jetbrains.kotlin.jvm")
        // `java-library` is what publishes a usable runtime variant for other projects to
        // consume. Without it an Android module gets these classes on its compile
        // classpath but not on its unit-test runtime classpath, which fails at class-load
        // time inside a test rather than at build time — a long way from the cause.
        pluginManager.apply("java-library")

        extensions.configure<JavaPluginExtension> {
            sourceCompatibility = JavaVersion.VERSION_17
            targetCompatibility = JavaVersion.VERSION_17
        }

        tasks.withType<KotlinCompile>().configureEach {
            compilerOptions {
                jvmTarget.set(JvmTarget.JVM_17)
                allWarningsAsErrors.set(true)
            }
        }

        tasks.withType<Test>().configureEach { useJUnitPlatform() }

        dependencies {
            add("testImplementation", platform(libs.lib("junit5-bom")))
            listOf("junit5-jupiter", "turbine", "mockk", "kotlinx-coroutines-test").forEach {
                add("testImplementation", libs.lib(it))
            }
            add("testRuntimeOnly", libs.lib("junit5-platform-launcher"))
        }
    }
}
