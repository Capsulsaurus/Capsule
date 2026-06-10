# Hardware-bound device keys — native implementer guide

The device signing key (DSK) is hardware-bound: its private half never leaves the platform
secure element (Secure Enclave / StrongBox / TPM). This guide is for the **per-platform native
follow-up** — the Rust seam, the FFI foreign trait, and the in-process smoke contract already
exist (`capsule-core::crypto::keys::{signer, hardware}`, exposed over uniffi under the `ffi`
feature). Design SSoT: [`cryptography/keys.md`](../capsule-docs/src/content/docs/design/cryptography/keys.md).

## The classical / post-quantum split

No shipping secure element holds ML-DSA-65 keys, so the device key is bound to hardware only on
its **classical** half:

```
DSK = Ed25519 (in the secure element, non-exportable)  ‖  ML-DSA-65 (software-sealed ξ seed)
sign(msg) = HW_ed25519_sig  ‖  SW_mldsa_sig
```

Rust composes the two halves into the hybrid signature
(`HardwareBackedSigner`); native code only implements the Ed25519 side.

## The contract native code implements

uniffi generates a `HardwareSigner` protocol (Swift) / interface (Kotlin). A conforming
implementation:

| Method | Returns | Notes |
| --- | --- | --- |
| `enroll(keyAlias)` | 32-byte Ed25519 public key | Generate-or-bind, **non-exportable**, idempotent per alias. |
| `classicalPublicKey(keyAlias)` | 32-byte Ed25519 public key | For an already-enrolled alias. |
| `signClassical(keyAlias, msg)` | 64-byte Ed25519 signature | Signs the raw bytes Rust hands it. |
| `assertNonExportable(keyAlias)` | — (throws) | Attempt to read the private bytes; **must** throw `.exportable` if they are readable. |

Pass the implementation to `FfiWorkspace.createWithHardwareSigner(root, passphrase, tier,
hardware, keyAlias, mlSeed)`, where `mlSeed` is the 32-byte software ML-DSA ξ seed.

### Apple — Secure Enclave (stub)

```swift
final class SecureEnclaveSigner: HardwareSigner {
    func enroll(keyAlias: String) throws -> Data { /* SecKeyCreateRandomKey(kSecAttrTokenIDSecureEnclave), return raw pubkey */ }
    func classicalPublicKey(keyAlias: String) throws -> Data { /* SecKeyCopyExternalRepresentation of the public key */ }
    func signClassical(keyAlias: String, msg: Data) throws -> Data { /* SecKeyCreateSignature(privateKey, .ed25519, msg) */ }
    func assertNonExportable(keyAlias: String) throws { /* SecKeyCopyExternalRepresentation(privateKey) must fail → ok; else throw .exportable */ }
}
```
> Note: the Secure Enclave exposes P-256, not Ed25519, on most OS versions; a production adapter
> either uses a CryptoKit `SecureEnclave.P256` keypair (and Capsule pins a P-256 classical half
> for hardware devices) or a Keychain-backed Curve25519 key with the non-extractable flag. The
> trait shape is unchanged; only the underlying `SecKey` algorithm differs.

### Android — StrongBox / Keystore (stub)

```kotlin
class StrongBoxSigner : HardwareSigner {
    override fun enroll(keyAlias: String): ByteArray { /* KeyPairGenerator(Ed25519, AndroidKeyStore) with setIsStrongBoxBacked(true) */ }
    override fun classicalPublicKey(keyAlias: String): ByteArray { /* keyStore.getCertificate(alias).publicKey.encoded */ }
    override fun signClassical(keyAlias: String, msg: ByteArray): ByteArray { /* Signature.getInstance("Ed25519") with the AndroidKeyStore PrivateKey */ }
    override fun assertNonExportable(keyAlias: String) { /* KeyInfo.isInsideSecureHardware must be true; key bytes are never retrievable → ok */ }
}
```

## Per-platform smoke harness (`keys.md` Validation)

Each platform ships a smoke test: **generate the DSK in the element → sign a fixed payload →
verify against the published public key → assert the private bytes are unreadable.** The Rust
in-process equivalent (against a mock element) already runs in CI:
`capsule-core::crypto::keys::hardware::tests` and `lifecycle::tests::hardware_backed_device_imports_and_verifies`.

## Still deferred

- Wiring the generated bindings + `cdylib`/`staticlib` into the Xcode / Gradle builds.
- The real Secure Enclave / StrongBox / TPM adapters and their on-device smoke tests.
- Hardware binding of the device **encryption** key (DEK); only signing is covered here.
