// Included build carrying the Gradle convention plugins the Android modules share.
// Keeping it a separate build (rather than buildSrc) means editing a convention plugin
// does not invalidate the whole main build's configuration cache.
dependencyResolutionManagement {
    repositories {
        google()
        mavenCentral()
    }
    versionCatalogs {
        create("libs") {
            from(files("../../gradle/libs.versions.toml"))
        }
    }
}

rootProject.name = "build-logic"
