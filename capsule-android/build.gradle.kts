plugins {
    // AGP 9 supplies Kotlin itself; org.jetbrains.kotlin.android is a hard error here.
    alias(libs.plugins.androidApplication)
    alias(libs.plugins.composeCompiler)
}

android {
    namespace = "com.justin13888.capsule"
    compileSdk = libs.versions.compileSdk.get().toInt()

    defaultConfig {
        applicationId = "com.justin13888.capsule"
        minSdk = libs.versions.minSdk.get().toInt()
        targetSdk = libs.versions.targetSdk.get().toInt()
        // Repo-wide version source of truth: gradle.properties, kept in sync by
        // `mise run set-version` (xtask), which also bumps versionCode on release.
        versionCode = providers.gradleProperty("capsule.versionCode").get().toInt()
        versionName = providers.gradleProperty("capsule.versionName").get()
    }

    buildFeatures {
        compose = true
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    sourceSets["main"].manifest.srcFile("src/androidMain/AndroidManifest.xml")
    sourceSets["main"].kotlin.srcDir("src/androidMain/kotlin")
    sourceSets["main"].res.srcDir("src/androidMain/res")
}

kotlin {
    compilerOptions {
        jvmTarget.set(org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17)
    }
}

dependencies {
    implementation(platform(libs.compose.bom))
    implementation(libs.compose.foundation)
    implementation(libs.compose.material3)
    implementation(libs.compose.ui)
    implementation(libs.androidx.activity.compose)
    implementation(libs.androidx.core.ktx)

    debugImplementation(libs.compose.ui.tooling)
}
