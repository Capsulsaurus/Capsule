# capsule-core-swift

A standalone SwiftPM harness that links the compiled `capsule-sdk` over its `capsule_sdk` uniffi
bindings (the app-facing namespace, which reaches `capsule-core` for every cryptographic step) and
proves it works from Swift **before** any iOS/macOS app integration. It ships the per-platform
`FfiHardwareSigner` and `FfiHardwareKeyAgreement` references and a smoke test.

| File | What it is |
| --- | --- |
| `Sources/CapsuleHardware/SoftwareSigner.swift` | Software fallback (CryptoKit Curve25519). Genuine Ed25519 → composes into the hybrid DSK end to end. |
| `Sources/CapsuleHardware/SoftwareP256Signer.swift` | Software P-256 reference (CryptoKit). Mirrors `SecureEnclaveSigner`'s wire contract, so the P-256 hybrid DSK runs on a CI VM. |
| `Sources/CapsuleHardware/SecureEnclaveSigner.swift` | Real Secure Enclave adapter (CryptoKit P-256). See the algorithm notes in the file. |
| `Sources/CapsuleHardware/SoftwareP256KeyAgreement.swift` | Software P-256 ECDH reference for the hybrid device encryption key. |
| `Sources/CapsuleHardware/SecureEnclaveKeyAgreement.swift` | Real Secure Enclave ECDH adapter for the hybrid device encryption key. |
| `Tests/CapsuleHardwareTests/SmokeTests.swift` | Creates an `FfiWorkspace` (software + hardware-signer paths), seals and verifies an asset, round-trips the hardware DEK; exercises the Secure Enclave on-device. |

The generated `capsule_sdk.swift` and `capsule_sdkFFI.h` are **not** committed; `stage-bindings.sh`
emits them (they are `.gitignore`d).

## Test it (macOS, the documented dev machine)

```sh
cd capsule-core-swift
./stage-bindings.sh        # builds libcapsule_sdk.dylib + stages the generated bindings
swift test                 # software paths run anywhere; the Secure Enclave tests run on
                           # Apple-Silicon / T2 Macs and are skipped where no SE is present
```

`./stage-bindings.sh` runs `mise run gen-bindings` at the repo root, which builds
`target/debug/libcapsule_sdk.dylib`; `Package.swift` links it by absolute path with an `rpath`, so
`swift test` needs no `DYLD_*` setup.

## Status / follow-ups

- The **software** paths are exercised end to end (the FFI + the hardware-signer and
  key-agreement foreign traits).
- The **Secure Enclave** adapters are P-256 (the Enclave has no Ed25519), so they plug into
  `createWithP256HardwareSigner` and `p256HardwareDekRoundTrip`, not the Ed25519
  `createWithHardwareSigner` path. Wiring the bindings into the real `capsule-swift` Xcode app is
  a follow-up.
