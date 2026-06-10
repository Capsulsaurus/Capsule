// Standalone build root so this harness is testable on its own (the repo's root multi-project
// build is a separate, app-oriented concern). Run from this directory: `./gradlew test`.
pluginManagement {
    repositories {
        google()
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "capsule-core-kotlin"
