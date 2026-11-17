plugins {
    alias(libs.plugins.androidApplication) apply false
    alias(libs.plugins.androidLibrary) apply false
    alias(libs.plugins.composeCompiler) apply false
    alias(libs.plugins.kotlinAndroid) apply false
    alias(libs.plugins.kotlinxSerialization) apply false
    alias(libs.plugins.ktlint) apply false
    alias(libs.plugins.detekt) apply false
}

subprojects {
    // The plugins that compile a module's Kotlin. Under AGP 9 that is the Android plugin itself
    // (built-in Kotlin); `org.jetbrains.kotlin.android` is no longer applied anywhere.
    listOf(
        "com.android.application",
        "com.android.library",
        "com.android.test",
        "org.jetbrains.kotlin.jvm"
    ).forEach { kotlinPlugin ->
        pluginManager.withPlugin(kotlinPlugin) {
            apply(plugin = "org.jlleitschuh.gradle.ktlint")
            apply(plugin = "io.gitlab.arturbosch.detekt")

            // ktlint: don't lint generated sources: what a plugin writes under build/generated/
            // and `:core`'s staged uniffi bindings under build/generated-bindings/. Rules
            // themselves are configured via the root .editorconfig.
            extensions.configure(org.jlleitschuh.gradle.ktlint.KtlintExtension::class.java) {
                filter {
                    exclude { it.file.path.contains("/build/generated") }
                }
            }

            // Detekt: merge the shared root config on top of detekt's bundled defaults.
            extensions.configure(io.gitlab.arturbosch.detekt.extensions.DetektExtension::class.java) {
                buildUponDefaultConfig = true
                config.setFrom(rootProject.file("detekt.yml"))
            }
            // ...and likewise keep detekt off generated sources.
            tasks.withType(io.gitlab.arturbosch.detekt.Detekt::class.java).configureEach {
                exclude("**/build/generated*/**")
            }
        }
    }
}
