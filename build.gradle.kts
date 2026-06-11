plugins {
    alias(libs.plugins.androidApplication) apply false
    alias(libs.plugins.androidLibrary) apply false
    alias(libs.plugins.composeCompiler) apply false
    alias(libs.plugins.composeMultiplatform) apply false
    alias(libs.plugins.kotlinMultiplatform) apply false
    alias(libs.plugins.kotlinxSerialization) apply false
    alias(libs.plugins.kmpNativeCoroutines) apply false
    alias(libs.plugins.ksp) apply false
    alias(libs.plugins.ktlint) apply false
    alias(libs.plugins.detekt) apply false
}

subprojects {
    listOf(
        "org.jetbrains.kotlin.multiplatform",
        "org.jetbrains.kotlin.android",
        "org.jetbrains.kotlin.jvm"
    ).forEach { kotlinPlugin ->
        pluginManager.withPlugin(kotlinPlugin) {
            apply(plugin = "org.jlleitschuh.gradle.ktlint")
            apply(plugin = "io.gitlab.arturbosch.detekt")

            // Detekt: merge the shared root config on top of detekt's bundled defaults.
            // ktlint rules are configured via the root .editorconfig.
            extensions.configure(io.gitlab.arturbosch.detekt.extensions.DetektExtension::class.java) {
                buildUponDefaultConfig = true
                config.setFrom(rootProject.file("detekt.yml"))
            }
        }
    }
}
