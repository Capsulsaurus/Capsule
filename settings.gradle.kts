rootProject.name = "Capsule"
enableFeaturePreview("TYPESAFE_PROJECT_ACCESSORS")

pluginManagement {
    // Convention plugins shared by every Android module. An included build rather than
    // buildSrc, so editing one does not invalidate the main build's configuration cache.
    includeBuild("capsule-android/build-logic")

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

// ── The Compose app ──────────────────────────────────────────────────────────
include(":capsule-android:app")

// Shared building blocks. `model` and `data` are pure JVM: the domain types and the
// repository contracts have no reason to know about Android, and their tests run
// without an Android runtime.
include(":capsule-android:core:model")
include(":capsule-android:core:data")
include(":capsule-android:core:mock")
include(":capsule-android:core:designsystem")
include(":capsule-android:core:ui")
include(":capsule-android:core:i18n")
include(":capsule-android:core:testing")

// One module per product area. Features depend on core, never on each other.
include(":capsule-android:feature:onboarding")
include(":capsule-android:feature:enrollment")
include(":capsule-android:feature:timeline")
include(":capsule-android:feature:importer")
include(":capsule-android:feature:albums")
include(":capsule-android:feature:viewer")
include(":capsule-android:feature:search")
include(":capsule-android:feature:people")
include(":capsule-android:feature:sharing")
include(":capsule-android:feature:drops")
include(":capsule-android:feature:sync")
include(":capsule-android:feature:storage")
include(":capsule-android:feature:security")
include(":capsule-android:feature:recovery")
include(":capsule-android:feature:moderation")
include(":capsule-android:feature:settings")
include(":capsule-android:feature:audit")

// ── The uniffi harness ───────────────────────────────────────────────────────
// capsule-core-kotlin links the compiled capsule-core over its uniffi bindings. It is
// deliberately NOT a dependency of the app: the app is built against mock repositories
// until the FFI surface stabilises.
include(":core")
project(":core").projectDir = file("capsule-core-kotlin")
