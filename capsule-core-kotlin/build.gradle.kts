plugins {
    // Versions are managed by the root build (apply false); re-declaring them here
    // conflicts with the plugin already on the classpath.
    id("com.android.library")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "com.justin13888.capsule.core"
    compileSdk = 36

    defaultConfig {
        minSdk = 26
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    // The generated uniffi Kotlin bindings, staged by ./stage-bindings.sh (.gitignored).
    sourceSets["main"].kotlin.srcDir("build/generated-bindings")
    // Per-ABI JNI libs (cargo-ndk output), staged for on-device instrumented tests.
    sourceSets["main"].jniLibs.srcDir("src/main/jniLibs")
}

kotlin {
    jvmToolchain(17)
}

dependencies {
    // uniffi Kotlin bindings need JNA at runtime. The plain jar carries the host jnidispatch the
    // JVM unit test needs; on-device instrumented tests use the @aar (per-ABI Android natives).
    implementation("net.java.dev.jna:jna:5.14.0")
    implementation("org.bouncycastle:bcprov-jdk18on:1.78.1")

    testImplementation("org.junit.jupiter:junit-jupiter:5.10.2")

    androidTestImplementation("net.java.dev.jna:jna:5.14.0@aar")
    androidTestImplementation("androidx.test.ext:junit:1.2.1")
    androidTestImplementation("androidx.test:runner:1.6.2")
}

// JVM unit tests load the host libcapsule_core dylib/so through JNA. The repo root is
// this module's parent whether it builds standalone (module == Gradle root) or as the
// app build's `:core` (root == repo) — `rootDir` differs between the two, projectDir
// does not, so resolve target/ off projectDir.
tasks.withType<Test>().configureEach {
    useJUnitPlatform()
    systemProperty("jna.library.path", "${projectDir.parentFile}/target/debug")
}

// ── FFI wiring (S-F3) ────────────────────────────────────────────────────────
// The uniffi Kotlin bindings (build/generated-bindings) and the per-ABI JNI libs
// (src/main/jniLibs) are generated build inputs — both .gitignored, never committed.
// These tasks reproduce what stage-bindings.sh does by hand so a clean-checkout Gradle
// build (app assemble, JVM unit tests, instrumented tests) is self-contained instead of
// depending on a prior manual staging step.
//
// They shell out to cargo / mise, so they are OFF by default: the ktlint/detekt lane
// configures :core with no Rust toolchain present and must stay that way. Opt in with
// `-Pcapsule.wireFfi` (the `build-kotlin` / `test-kotlin` mise tasks and the Android
// app-build CI lane pass it; the lint-only lane does not).
//
// projectDir is always this module's dir in both the standalone build (this module is
// the Gradle root) and the app build (included as `:core`), so the repo root is its
// parent in either case — unlike `rootDir`, which differs between the two.
val repoRoot = projectDir.parentFile

val stageUniffiBindings by tasks.registering(Exec::class) {
    description = "Generate + stage the uniffi Kotlin bindings (mise run gen-bindings)"
    group = "ffi"
    workingDir = projectDir
    // Idempotent: builds target/debug/libcapsule_core + copies capsule_core.kt into
    // build/generated-bindings/uniffi/capsule_core/ (the sourceSet srcDir above).
    commandLine("bash", "stage-bindings.sh")
    outputs.dir(layout.buildDirectory.dir("generated-bindings"))
}

// cargo-ndk's `-o` lays the per-ABI .so out in the jniLibs directory layout AGP expects
// (<abi>/libcapsule_core.so). arm64/armv7/x86_64/x86 cover the four Tier-1 Android ABIs.
val buildAndroidJniLibs by tasks.registering(Exec::class) {
    description = "Build per-ABI libcapsule_core.so via cargo-ndk into src/main/jniLibs"
    group = "ffi"
    workingDir = repoRoot
    val jniLibsDir = file("src/main/jniLibs")
    val abiArgs = listOf("arm64-v8a", "armeabi-v7a", "x86_64", "x86").flatMap { listOf("-t", it) }
    commandLine(
        listOf("cargo", "ndk", "-o", jniLibsDir.absolutePath) + abiArgs +
            listOf("--platform", "26", "build", "-p", "capsule-core", "--features", "ffi"),
    )
    outputs.dir(jniLibsDir)
}

if (project.hasProperty("capsule.wireFfi")) {
    // Bindings are Kotlin source: every compilation (JVM unit tests + Android variants)
    // needs them, so hang them off preBuild, which all compile paths depend on.
    tasks.named("preBuild").configure { dependsOn(stageUniffiBindings) }
    // JNI libs are only needed when a native artifact is actually produced or run:
    // the APK's merged jniLibs and on-device instrumented tests. Keeping them off the
    // compile path means a plain JVM `test` (host dylib via JNA) needs no NDK.
    tasks.matching {
        (it.name.startsWith("merge") && it.name.endsWith("JniLibFolders")) ||
            it.name.startsWith("connected") ||
            it.name.startsWith("bundle") && it.name.endsWith("Aar")
    }.configureEach { dependsOn(buildAndroidJniLibs) }
}
