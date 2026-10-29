import org.jetbrains.kotlin.gradle.ExperimentalKotlinGradlePluginApi
import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    alias(libs.plugins.kotlinMultiplatform)
    alias(libs.plugins.androidApplication)
    alias(libs.plugins.composeCompiler)
    alias(libs.plugins.composeMultiplatform)
    alias(libs.plugins.kotlinxSerialization)
}

kotlin {
    androidTarget {
        @OptIn(ExperimentalKotlinGradlePluginApi::class)
        compilerOptions {
            jvmTarget.set(JvmTarget.JVM_17)
        }
    }

    sourceSets {
        androidMain.dependencies {
            implementation(libs.androidx.compose.ui.tooling.preview)
            implementation(libs.androidx.activity.compose)
            // The in-app language picker below Android 13 (AppCompatDelegate.setApplicationLocales).
            implementation(libs.androidx.appcompat)
            implementation(libs.androidx.viewmodel.compose)
            implementation(libs.androidx.navigation.compose)
            implementation(libs.koin.androidx.compose)
            implementation(libs.coil.compose)
            implementation(libs.coil.network.ktor)
            // JankStats (field frame telemetry) and the trace sections the parity benchmark reads.
            implementation(libs.androidx.metrics.performance)
            implementation(libs.androidx.tracing.ktx)
        }
        // Plain JVM unit tests over the alert loop, which touches no Android API.
        androidUnitTest.dependencies {
            implementation(kotlin("test"))
        }
        commonMain.dependencies {
            implementation(compose.runtime)
            implementation(compose.foundation)
            implementation(compose.material3)
            implementation(compose.ui)
            implementation(compose.components.resources)
            implementation(compose.components.uiToolingPreview)
            implementation(projects.core)
            implementation(libs.kotlinx.serialization.core)
        }
    }
}

android {
    namespace = "com.justin13888.capsule"
    compileSdk = 36

    defaultConfig {
        applicationId = "com.justin13888.capsule"
        minSdk = 26
        targetSdk = 36
        // Version is the repo-wide source of truth in gradle.properties, kept in sync
        // across every package by `mise run set-version` (xtask).
        versionCode = providers.gradleProperty("capsule.versionCode").get().toInt()
        versionName = providers.gradleProperty("capsule.versionName").get()
    }
    packaging {
        resources {
            excludes += "/META-INF/{AL2.0,LGPL2.1}"
        }
    }
    buildTypes {
        getByName("release") {
            isMinifyEnabled = false
        }
        // What the parity benchmark (`:benchmark`) measures: release code, never debuggable
        // (a debuggable build runs interpreted and would time the wrong app), signed with the
        // debug key so a bench device installs it without a release keystore.
        create("benchmark") {
            initWith(getByName("release"))
            signingConfig = signingConfigs.getByName("debug")
            matchingFallbacks += listOf("release")
            isDebuggable = false
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

dependencies {
    debugImplementation(libs.androidx.compose.ui.tooling)
}
