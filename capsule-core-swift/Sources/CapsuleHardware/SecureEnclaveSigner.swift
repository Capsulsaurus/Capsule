import CryptoKit
import Foundation

/// A Secure Enclave–backed ``HardwareSigner`` (Apple, iOS / macOS). The private key is generated
/// inside the Secure Enclave and never leaves it — there is no API to read the private bytes, so
/// non-exportability is enforced by the platform.
///
/// ## Algorithm (the same one StrongBox and the TPM reference produce)
///
/// The Secure Enclave only does **NIST P-256**, not Ed25519, so this returns P-256 material: a
/// 65-byte x9.63 (uncompressed SEC1) public key and a **DER-encoded ECDSA** signature over `msg`
/// (CryptoKit hashes with SHA-256 internally). That is exactly what `P256HybridSigningKey` consumes
/// on the Rust side — the element's public key normalizes from SEC1, and the DER signature verifies
/// verbatim via `p256::ecdsa` — so this plugs into the P-256
/// `FfiWorkspace.createWithP256HardwareSigner` path end to end (slice S-F2). The Ed25519
/// ``SoftwareSigner`` drives the separate `createWithHardwareSigner` path.
///
/// A production app persists `dataRepresentation` of each key (the encrypted SE blob) in the
/// Keychain and reloads it; this reference keeps the handles in memory.
public final class SecureEnclaveSigner: HardwareSigner, @unchecked Sendable {
    private var keys: [String: SecureEnclave.P256.Signing.PrivateKey] = [:]
    private let lock = NSLock()

    public init() {}

    private func privateKey(_ keyAlias: String) throws -> SecureEnclave.P256.Signing.PrivateKey {
        lock.lock()
        defer { lock.unlock() }
        if let existing = keys[keyAlias] {
            return existing
        }
        guard SecureEnclave.isAvailable else {
            throw HardwareSignerError.Unavailable(message: "Secure Enclave unavailable")
        }
        let key = try SecureEnclave.P256.Signing.PrivateKey()
        keys[keyAlias] = key
        return key
    }

    public func enroll(keyAlias: String) throws -> Data {
        try privateKey(keyAlias).publicKey.x963Representation
    }

    public func classicalPublicKey(keyAlias: String) throws -> Data {
        try privateKey(keyAlias).publicKey.x963Representation
    }

    public func signClassical(keyAlias: String, msg: Data) throws -> Data {
        // DER-encoded ECDSA — the form `P256HybridSigningKey` parses verbatim on the Rust side.
        try privateKey(keyAlias).signature(for: msg).derRepresentation
    }

    public func assertNonExportable(keyAlias: String) throws {
        // The Secure Enclave exposes no API to read the private key, so possession of an
        // SE-resident key is itself the non-exportability guarantee. Confirm it is resident.
        _ = try privateKey(keyAlias)
    }
}
