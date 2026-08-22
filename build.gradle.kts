plugins {
    alias(libs.plugins.androidApplication) apply false
    alias(libs.plugins.androidLibrary) apply false
    alias(libs.plugins.androidTest) apply false
    alias(libs.plugins.composeCompiler) apply false
    alias(libs.plugins.kotlinJvm) apply false
    alias(libs.plugins.kotlinxSerialization) apply false
    alias(libs.plugins.ktlint) apply false
    alias(libs.plugins.detekt) apply false
    alias(libs.plugins.roborazzi) apply false
}

// AGP 9 carries built-in Kotlin support, so an Android module never applies
// org.jetbrains.kotlin.android — the plugin ids below are the ones that survive:
// AGP's own (which register the Kotlin compilations) and the JVM-only Kotlin plugin.
subprojects {
    listOf(
        "com.android.application",
        "com.android.library",
        "com.android.test",
        "org.jetbrains.kotlin.jvm",
    ).forEach { hostPlugin ->
        pluginManager.withPlugin(hostPlugin) {
            apply(plugin = "org.jlleitschuh.gradle.ktlint")
            apply(plugin = "io.gitlab.arturbosch.detekt")

            // ktlint: don't lint generated sources (Compose resource accessors, the
            // staged uniffi bindings). Rules themselves come from the root .editorconfig.
            extensions.configure(org.jlleitschuh.gradle.ktlint.KtlintExtension::class.java) {
                filter {
                    exclude { it.file.path.contains("/build/generated/") }
                }
            }

            // Detekt: merge the shared root config on top of detekt's bundled defaults.
            extensions.configure(io.gitlab.arturbosch.detekt.extensions.DetektExtension::class.java) {
                buildUponDefaultConfig = true
                config.setFrom(rootProject.file("detekt.yml"))
            }
            tasks.withType(io.gitlab.arturbosch.detekt.Detekt::class.java).configureEach {
                exclude("**/build/generated/**")
            }
        }
    }
}
