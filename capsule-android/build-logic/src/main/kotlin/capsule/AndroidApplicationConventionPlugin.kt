package capsule

import com.android.build.api.dsl.ApplicationExtension
import org.gradle.api.Plugin
import org.gradle.api.Project
import org.gradle.kotlin.dsl.configure

/** The single Android application module. */
class AndroidApplicationConventionPlugin : Plugin<Project> {
    override fun apply(target: Project) = with(target) {
        pluginManager.apply("com.android.application")
        pluginManager.apply("capsule.android.compose")
        pluginManager.apply("capsule.android.test")

        extensions.configure<ApplicationExtension> {
            configureAndroidBase(this)
            defaultConfig {
                targetSdk = libs.int("targetSdk")
                // Repo-wide version source of truth: gradle.properties, kept in sync by
                // `mise run set-version` (xtask), which also bumps versionCode.
                versionCode = providers.gradleProperty("capsule.versionCode").get().toInt()
                versionName = providers.gradleProperty("capsule.versionName").get()
            }
        }
    }
}
