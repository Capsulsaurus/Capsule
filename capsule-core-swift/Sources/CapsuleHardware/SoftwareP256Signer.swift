import CryptoKit
import Foundation

/// A software **P-256** ``HardwareSigner`` — the no-secure-element reference the real Secure
/// Enclave replaces, and the always-runnable path for the P-256 hybrid composition (a CI VM has no
/// Secure Enclave). It mirrors ``SecureEnclaveSigner``'s wire contract exactly — a 65-byte x9.63
/// (uncompressed SEC1) public key and a **DER-encoded ECDSA** signature over `msg` (SHA-256 hashed
/// internally) — so it plugs into `FfiWorkspace.createWithP256HardwareSigner` end to end (slice
/// S-F2). The private key lives in process memory, derived deterministically (HKDF-SHA256, per
/// `keyAlias`) from a 32-byte seed the caller seals.
///
/// It offers no hardware non-exportability, which ``assertNonExportable(keyAlias:)`` reports
/// truthfully by throwing `.Exportable`.
public final class SoftwareP256Signer: HardwareSigner, @unchecked Sendable {
    private static let info = Data("capsule/software-signer/p256/v1".utf8)
    private let seed: Data

    /// Build a signer from a 32-byte seed. Seal `seed` (e.g. in the Keychain) so the device key
    /// survives restarts.
    public init(seed: Data) {
        precondition(seed.count == 32, "software signer seed must be 32 bytes")
        self.seed = seed
    }

    private func key(_ keyAlias: String) throws -> P256.Signing.PrivateKey {
        let derived = HKDF<SHA256>.deriveKey(
            inputKeyMaterial: SymmetricKey(data: seed),
            salt: Data(keyAlias.utf8),
            info: Self.info,
            outputByteCount: 32
        )
        return try derived.withUnsafeBytes {
            try P256.Signing.PrivateKey(rawRepresentation: Data($0))
        }
    }

    public func enroll(keyAlias: String) throws -> Data {
        try classicalPublicKey(keyAlias: keyAlias)
    }

    public func classicalPublicKey(keyAlias: String) throws -> Data {
        try key(keyAlias).publicKey.x963Representation
    }

    public func signClassical(keyAlias: String, msg: Data) throws -> Data {
        try key(keyAlias).signature(for: msg).derRepresentation
    }

    public func assertNonExportable(keyAlias: String) throws {
        // Honest by design: a software key is readable, so it can never meet the hardware
        // non-exportability contract a Secure Enclave / StrongBox / TPM does.
        throw HardwareSignerError.Exportable(message: "software P-256 key is exportable")
    }
}
