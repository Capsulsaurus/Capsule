plugins {
    `kotlin-dsl`
}

// The convention plugins call into AGP's and Kotlin's Gradle APIs, so both plugin
// artifacts sit on this build's compile classpath. They are `compileOnly` because the
// consuming build supplies them at runtime from its own plugin classpath.
dependencies {
    compileOnly(libs.agp.gradle.plugin)
    compileOnly(libs.kotlin.gradle.plugin)
    compileOnly(libs.compose.compiler.gradle.plugin)
}

kotlin {
    compilerOptions {
        jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17)
    }
}

gradlePlugin {
    plugins {
        register("androidApplication") {
            id = "capsule.android.application"
            implementationClass = "capsule.AndroidApplicationConventionPlugin"
        }
        register("androidLibrary") {
            id = "capsule.android.library"
            implementationClass = "capsule.AndroidLibraryConventionPlugin"
        }
        register("androidCompose") {
            id = "capsule.android.compose"
            implementationClass = "capsule.AndroidComposeConventionPlugin"
        }
        register("androidFeature") {
            id = "capsule.android.feature"
            implementationClass = "capsule.AndroidFeatureConventionPlugin"
        }
        register("androidTest") {
            id = "capsule.android.test"
            implementationClass = "capsule.AndroidTestConventionPlugin"
        }
        register("jvmLibrary") {
            id = "capsule.jvm.library"
            implementationClass = "capsule.JvmLibraryConventionPlugin"
        }
    }
}
