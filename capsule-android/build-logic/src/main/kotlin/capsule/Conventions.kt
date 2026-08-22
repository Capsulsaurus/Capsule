package capsule

import com.android.build.api.dsl.CommonExtension
import org.gradle.api.JavaVersion
import org.gradle.api.Project
import org.gradle.api.artifacts.VersionCatalog
import org.gradle.api.artifacts.VersionCatalogsExtension
import org.gradle.api.artifacts.dsl.DependencyHandler
import org.gradle.api.artifacts.MinimalExternalModuleDependency
import org.gradle.api.provider.Provider
import org.gradle.kotlin.dsl.dependencies
import org.gradle.kotlin.dsl.getByType
import org.gradle.kotlin.dsl.withType
import org.jetbrains.kotlin.gradle.dsl.JvmTarget
import org.jetbrains.kotlin.gradle.tasks.KotlinCompile

/** The version catalog, reachable from a plugin where the `libs` accessor is not. */
internal val Project.libs: VersionCatalog
    get() = extensions.getByType<VersionCatalogsExtension>().named("libs")

internal fun VersionCatalog.lib(alias: String): Provider<MinimalExternalModuleDependency> =
    findLibrary(alias).orElseThrow { IllegalArgumentException("no library alias '$alias' in libs.versions.toml") }

internal fun VersionCatalog.int(alias: String): Int =
    findVersion(alias).orElseThrow { IllegalArgumentException("no version alias '$alias' in libs.versions.toml") }
        .requiredVersion
        .toInt()

/**
 * SDK levels, Java level and compiler strictness shared by every Android module.
 *
 * Kotlin options are set on the compile tasks rather than through the `kotlin`
 * extension: AGP 9 registers the Kotlin compilations itself, and going through the
 * tasks keeps this working regardless of which extension type it installs.
 */
internal fun Project.configureAndroidBase(extension: CommonExtension) {
    extension.apply {
        compileSdk = libs.int("compileSdk")
        defaultConfig.minSdk = libs.int("minSdk")

        // AGP 9 dropped the action overload; the property is still there.
        compileOptions.sourceCompatibility = JavaVersion.VERSION_17
        compileOptions.targetCompatibility = JavaVersion.VERSION_17
    }

    tasks.withType<KotlinCompile>().configureEach {
        compilerOptions {
            jvmTarget.set(JvmTarget.JVM_17)
            // The Android lanes are new, so they start warning-clean and stay that way.
            allWarningsAsErrors.set(true)
        }
    }
}

/** Adds the Compose dependencies every Compose surface in the app needs. */
internal fun Project.configureComposeDependencies() {
    dependencies {
        val bom = platform(libs.lib("compose-bom"))
        add("implementation", bom)
        add("androidTestImplementation", bom)
        add("testImplementation", bom)

        listOf(
            "compose-foundation",
            "compose-runtime",
            "compose-ui",
            "compose-ui-graphics",
            "compose-ui-text",
            "compose-material3",
            "androidx-lifecycle-runtime-compose",
            "androidx-lifecycle-viewmodel-compose",
            "kotlinx-collections-immutable",
            "compose-ui-tooling-preview",
        ).forEach { add("implementation", libs.lib(it)) }

        add("debugImplementation", libs.lib("compose-ui-tooling"))
    }
}

internal fun DependencyHandler.implementation(dependency: Any) {
    add("implementation", dependency)
}
