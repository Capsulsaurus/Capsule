import CryptoKit
import Foundation

/// A software ``HardwareSigner`` — the no-secure-element fallback, mirroring the Rust
/// `SoftwareSigner`. The classical Ed25519 key lives in process memory, derived deterministically
/// (HKDF-SHA512, per `keyAlias`) from a 32-byte seed the caller is responsible for sealing.
///
/// Because Curve25519 produces a genuine 32-byte Ed25519 public key and 64-byte signature, this
/// backend plugs **directly** into `FfiWorkspace.createWithHardwareSigner` — it is the one the
/// smoke test drives end to end. It offers no hardware non-exportability, which
/// ``assertNonExportable(keyAlias:)`` reports truthfully by throwing `.Exportable`.
///
/// The derivation matches the Rust reference (salt = `keyAlias`, info =
/// `capsule/software-signer/ed25519/v1`), so the same seed yields the same device key in either
/// language.
public final class SoftwareSigner: HardwareSigner, @unchecked Sendable {
    private static let info = Data("capsule/software-signer/ed25519/v1".utf8)
    private let seed: Data

    /// Build a signer from a 32-byte seed. Seal `seed` (e.g. in the Keychain) so the device key
    /// survives restarts.
    public init(seed: Data) {
        precondition(seed.count == 32, "software signer seed must be 32 bytes")
        self.seed = seed
    }

    private func key(_ keyAlias: String) throws -> Curve25519.Signing.PrivateKey {
        let derived = HKDF<SHA512>.deriveKey(
            inputKeyMaterial: SymmetricKey(data: seed),
            salt: Data(keyAlias.utf8),
            info: Self.info,
            outputByteCount: 32
        )
        return try derived.withUnsafeBytes { try Curve25519.Signing.PrivateKey(rawRepresentation: $0) }
    }

    public func enroll(keyAlias: String) throws -> Data {
        try classicalPublicKey(keyAlias: keyAlias)
    }

    public func classicalPublicKey(keyAlias: String) throws -> Data {
        try key(keyAlias).publicKey.rawRepresentation
    }

    public func signClassical(keyAlias: String, msg: Data) throws -> Data {
        try key(keyAlias).signature(for: msg)
    }

    public func assertNonExportable(keyAlias: String) throws {
        // Honest by design: a software key is readable, so it can never meet the hardware
        // non-exportability contract a Secure Enclave / StrongBox / TPM does.
        throw HardwareSignerError.Exportable(message: "software key is exportable")
    }
}
