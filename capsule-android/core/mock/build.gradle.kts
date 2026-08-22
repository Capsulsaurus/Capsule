plugins {
    id("capsule.android.library")
    // `capsule.android.test` pulls in Compose UI test, whose version comes from the Compose
    // BOM that `capsule.android.compose` installs. The module ships no Compose UI of its own;
    // the plugin is here so the test harness resolves.
    id("capsule.android.compose")
    id("capsule.android.test")
}

android {
    namespace = "com.justin13888.capsule.core.mock"
}

dependencies {
    // `implementation`, not `api`: nothing outside this module names a mock type in its own
    // signatures — a consumer wires the fakes in behind the `core:data` contracts and talks
    // to those. `core:data` re-exports `core:model` and `kotlinx-coroutines-core` as `api`,
    // so the domain types and `Flow` arrive transitively without being declared twice.
    implementation(projects.capsuleAndroid.core.data)
}
