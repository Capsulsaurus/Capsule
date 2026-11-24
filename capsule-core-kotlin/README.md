# capsule-core-kotlin

A standalone Android-library harness that links the compiled `capsule-sdk` over its `capsule_sdk`
uniffi bindings (the app-facing namespace, which reaches `capsule-core` for every cryptographic
step) and proves it works from Kotlin **before** any Android app integration. It ships the
per-platform `FfiHardwareSigner` references and a smoke test. (It is also the module the root
`settings.gradle.kts` wires as `:core`; here it builds on its own so it can be tested in isolation.)

| File | What it is |
| --- | --- |
| `src/main/kotlin/.../SoftwareSigner.kt` | Software fallback (BouncyCastle Ed25519 + HKDF-SHA512). Genuine Ed25519 → composes into the hybrid DSK end to end. |
| `src/main/kotlin/.../SoftwareP256Signer.kt` | Software P-256 reference (BouncyCastle ECDSA). Mirrors `StrongBoxSigner`'s wire contract, so the P-256 hybrid DSK runs on a JVM host. |
| `src/main/kotlin/.../StrongBoxSigner.kt` | Real AndroidKeyStore / StrongBox adapter (EC P-256). See the algorithm notes in the file. |
| `src/test/.../SoftwareSignerSmokeTest.kt` | JVM unit test: creates an `FfiWorkspace` (software + Ed25519 hardware-signer paths). |
| `src/test/.../SoftwareP256SignerSmokeTest.kt` | JVM unit test: the P-256 hardware-signer path, sealing and verifying an asset. |

The generated `capsule_sdk.kt` is **not** committed; `stage-bindings.sh` emits it under `build/`
(`.gitignore`d).

## Test it (the documented dev machine)

```sh
cd capsule-core-kotlin
./stage-bindings.sh        # builds libcapsule_sdk + stages the generated Kotlin bindings
./gradlew test             # JVM software smoke (JNA loads the host libcapsule_sdk)
```

Or let Gradle stage the bindings for you: `./gradlew test -Pcapsule.wireFfi` runs
the `stageUniffiBindings` task (equivalent to `./stage-bindings.sh`) automatically. The flag
is off by default so the lint-only lane (ktlint/detekt) needs no Rust toolchain; the repo's
`mise run test-kotlin` / `build-kotlin` pass it for you.

On-device StrongBox (a physical device with a secure element; an emulator only has a TEE):

```sh
# -Pcapsule.wireFfi also runs `buildAndroidJniLibs` (cargo-ndk → src/main/jniLibs/<abi>/):
./gradlew connectedAndroidTest -Pcapsule.wireFfi
```

## ⚠️ Toolchain prerequisites / known caveats

- **JDK vs Gradle.** This repo pins Gradle 9.9, which runs on JDK 17 through 27; CI runs it on
  JDK 21. This module compiles with a JDK 17 toolchain (`jvmToolchain(17)`), which Gradle must find
  installed, since no toolchain download repository is configured.
- **Android SDK.** `./gradlew` needs the Android SDK (`ANDROID_HOME` / `local.properties`).
- **StrongBox** is on-device only and (like Secure Enclave and the TPM) exposes ECDSA-P256, not
  Ed25519, so `StrongBoxSigner` plugs into `createWithP256HardwareSigner`, not the Ed25519
  `createWithHardwareSigner` path. `SoftwareP256Signer` drives the same P-256 path on a JVM host.
