# capsule-core-kotlin

A standalone Android-library harness that links the compiled `capsule-core` over its uniffi
bindings and proves it works from Kotlin **before** any Android app integration. It ships the
per-platform `HardwareSigner` references and a smoke test. (It is also the module the root
`settings.gradle.kts` wires as `:core`; here it builds on its own so it can be tested in isolation.)

| File | What it is |
| --- | --- |
| `src/main/kotlin/.../SoftwareSigner.kt` | Software fallback (BouncyCastle Ed25519 + HKDF-SHA512). Genuine Ed25519 → composes into the hybrid DSK end to end. |
| `src/main/kotlin/.../StrongBoxSigner.kt` | Real AndroidKeyStore / StrongBox adapter (EC P-256). See the algorithm caveat in the file. |
| `src/test/.../SoftwareSignerSmokeTest.kt` | JVM unit test: creates an `FfiWorkspace` (software + hardware-signer paths). |

The generated `capsule_core.kt` is **not** committed; `stage-bindings.sh` emits it under `build/`
(`.gitignore`d).

## Test it (the documented dev machine)

```sh
cd capsule-core-kotlin
./stage-bindings.sh        # builds libcapsule_core + stages the generated Kotlin bindings
./gradlew test             # JVM software smoke (JNA loads the host libcapsule_core)
```

On-device StrongBox (a physical device with a secure element; an emulator only has a TEE):

```sh
(cd .. && just build-android)      # per-ABI libcapsule_core.so via cargo-ndk
# copy the .so into src/main/jniLibs/<abi>/ (see stage-bindings.sh), then:
./gradlew connectedAndroidTest
```

## ⚠️ Toolchain prerequisites / known caveats

- **JDK vs Gradle.** This repo pins Gradle 8.11.1, which does **not** support JDK 26 (the version
  currently installed on the reference MacBook). Run with a JDK in Gradle 8.11.1's supported range
  (e.g. JDK 21): `JAVA_HOME=$(/usr/libexec/java_home -v 21) ./gradlew test`. This affects the whole
  repo's Gradle build, not just this module.
- **Android SDK.** `./gradlew` needs the Android SDK (`ANDROID_HOME` / `local.properties`).
- **StrongBox** is on-device only and (like Secure Enclave and the TPM) exposes ECDSA-P256, not
  Ed25519, so `StrongBoxSigner` does not yet wire into the Ed25519 `createWithHardwareSigner` path;
  that needs the P-256 hybrid-DSK variant tracked in the repo `DEFERRED.md`. Use `SoftwareSigner`
  for an end-to-end FFI round trip.
