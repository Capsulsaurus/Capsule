package capsule

import com.android.build.api.dsl.CommonExtension
import org.gradle.api.Plugin
import org.gradle.api.Project
import org.gradle.kotlin.dsl.getByType

/** Turns Compose on for an Android module and supplies the shared Compose libraries. */
class AndroidComposeConventionPlugin : Plugin<Project> {
    override fun apply(target: Project) = with(target) {
        pluginManager.apply("org.jetbrains.kotlin.plugin.compose")

        // Works for either host plugin; whichever registered the extension wins.
        val extension = extensions.getByType<CommonExtension>()
        extension.buildFeatures.compose = true

        configureComposeDependencies()
    }
}
