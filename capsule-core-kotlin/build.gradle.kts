plugins {
    id("com.android.library") version "8.9.2"
    id("org.jetbrains.kotlin.android") version "2.1.20"
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

// JVM unit tests load the host libcapsule_core dylib/so through JNA.
tasks.withType<Test>().configureEach {
    useJUnitPlatform()
    systemProperty("jna.library.path", "${rootDir}/../target/debug")
}
