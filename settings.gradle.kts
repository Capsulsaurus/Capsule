rootProject.name = "Capsule"
enableFeaturePreview("TYPESAFE_PROJECT_ACCESSORS")

pluginManagement {
    repositories {
        google {
            mavenContent {
                includeGroupAndSubgroups("androidx")
                includeGroupAndSubgroups("com.android")
                includeGroupAndSubgroups("com.google")
            }
        }
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositories {
        google {
            mavenContent {
                includeGroupAndSubgroups("androidx")
                includeGroupAndSubgroups("com.android")
                includeGroupAndSubgroups("com.google")
            }
        }
        mavenCentral()
    }
}

//plugins {
//    id("org.gradle.toolchains.foojay-resolver-convention") version "0.10.0"
//}

include(":android")
project(":android").projectDir = file("capsule-android")
//include(":capsule-core-kotlin")
include(":core")
project(":core").projectDir = file("capsule-core-kotlin")
include(":cli")
project(":cli").projectDir = file("capsule-cli")
include(":desktop")
project(":desktop").projectDir = file("capsule-desktop")
