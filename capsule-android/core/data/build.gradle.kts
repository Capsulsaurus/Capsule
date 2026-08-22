plugins {
    id("capsule.jvm.library")
}

dependencies {
    // `api`, not `implementation`: every repository signature here is spelled in domain
    // types, so a feature module that depends on `data` cannot compile without `model`.
    api(projects.capsuleAndroid.core.model)
    // Also `api` — `Flow` appears in the return type of most of these contracts.
    api(libs.kotlinx.coroutines.core)
}
