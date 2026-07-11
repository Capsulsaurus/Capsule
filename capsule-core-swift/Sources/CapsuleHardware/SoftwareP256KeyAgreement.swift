import CryptoKit
import Foundation

/// A software **P-256 ECDH** ``HardwareKeyAgreement`` — the no-secure-element reference the real
/// Secure Enclave replaces, and the always-runnable path for the hardware-bound device **encryption**
/// key's classical half (slice S-F5; a CI VM has no Secure Enclave). It mirrors
/// ``SecureEnclaveKeyAgreement``'s wire contract exactly — a 65-byte x9.63 (uncompressed SEC1)
/// public key, and ECDH that returns the **raw 32-byte shared secret** (the point's x-coordinate,
/// before any KDF) — so it plugs into `p256HardwareDekRoundTrip` end to end. The private key lives
/// in process memory, derived deterministically (HKDF-SHA256, per `keyAlias`) from a 32-byte seed
/// the caller seals.
///
/// It offers no hardware non-exportability, which ``assertNonExportable(keyAlias:)`` reports
/// truthfully by throwing `.Exportable`.
public final class SoftwareP256KeyAgreement: HardwareKeyAgreement, @unchecked Sendable {
    private static let info = Data("capsule/software-key-agreement/p256/v1".utf8)
    private let seed: Data

    /// Build an element from a 32-byte seed. Seal `seed` (e.g. in the Keychain) so the device key
    /// survives restarts.
    public init(seed: Data) {
        precondition(seed.count == 32, "software key-agreement seed must be 32 bytes")
        self.seed = seed
    }

    private func key(_ keyAlias: String) throws -> P256.KeyAgreement.PrivateKey {
        let derived = HKDF<SHA256>.deriveKey(
            inputKeyMaterial: SymmetricKey(data: seed),
            salt: Data(keyAlias.utf8),
            info: Self.info,
            outputByteCount: 32
        )
        return try derived.withUnsafeBytes {
            try P256.KeyAgreement.PrivateKey(rawRepresentation: Data($0))
        }
    }

    public func enroll(keyAlias: String) throws -> Data {
        try publicKey(keyAlias: keyAlias)
    }

    public func publicKey(keyAlias: String) throws -> Data {
        try key(keyAlias).publicKey.x963Representation
    }

    public func keyAgreement(keyAlias: String, peerPublic: Data) throws -> Data {
        let peer = try P256.KeyAgreement.PublicKey(x963Representation: peerPublic)
        let shared = try key(keyAlias).sharedSecretFromKeyAgreement(with: peer)
        // The raw shared secret is the 32-byte x-coordinate — exactly what the Rust hybrid combiner
        // folds in; no KDF is applied here.
        return shared.withUnsafeBytes { Data($0) }
    }

    public func assertNonExportable(keyAlias: String) throws {
        // Honest by design: a software key is readable, so it can never meet the hardware
        // non-exportability contract a Secure Enclave / StrongBox / TPM does.
        throw HardwareSignerError.Exportable(message: "software P-256 key is exportable")
    }
}
