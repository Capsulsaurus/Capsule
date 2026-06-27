# capsule-core-swift

A standalone SwiftPM harness that links the compiled `capsule-core` over its uniffi bindings and
proves it works from Swift **before** any iOS/macOS app integration. It ships the per-platform
`HardwareSigner` references and a smoke test.

| File | What it is |
| --- | --- |
| `Sources/CapsuleHardware/SoftwareSigner.swift` | Software fallback (CryptoKit Curve25519). Genuine Ed25519 → composes into the hybrid DSK end to end. |
| `Sources/CapsuleHardware/SecureEnclaveSigner.swift` | Real Secure Enclave adapter (CryptoKit P-256). See the algorithm caveat in the file. |
| `Tests/CapsuleHardwareTests/SmokeTests.swift` | Creates an `FfiWorkspace` (software + hardware-signer paths); exercises the Secure Enclave on-device. |

The generated `capsule_core.swift` and `capsule_coreFFI.h` are **not** committed; `stage-bindings.sh`
emits them (they are `.gitignore`d).

## Test it (macOS, the documented dev machine)

```sh
cd capsule-core-swift
./stage-bindings.sh        # builds libcapsule_core.dylib + stages the generated bindings
swift test                 # software paths run anywhere; the Secure Enclave test runs on
                           # Apple-Silicon / T2 Macs and is skipped where no SE is present
```

`./stage-bindings.sh` runs `mise run gen-bindings` at the repo root, which builds
`target/debug/libcapsule_core.dylib`; `Package.swift` links it by absolute path with an `rpath`, so
`swift test` needs no `DYLD_*` setup.

## Status / follow-ups

- The **software** path is exercised end to end (the FFI + the hardware-signer foreign trait).
- The **Secure Enclave** adapter is P-256 (the Enclave has no Ed25519), so it is not yet wired into
  the Ed25519 `createWithHardwareSigner` path; that needs the P-256 hybrid-DSK variant tracked in
  the repo `DEFERRED.md`. Wiring the bindings + dylib into the real `capsule-swift` Xcode app is
  also a follow-up.
