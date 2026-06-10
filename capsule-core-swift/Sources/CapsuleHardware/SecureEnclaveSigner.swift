import CryptoKit
import Foundation

/// A Secure Enclave–backed ``HardwareSigner`` (Apple, iOS / macOS). The private key is generated
/// inside the Secure Enclave and never leaves it — there is no API to read the private bytes, so
/// non-exportability is enforced by the platform.
///
/// ## Algorithm caveat (the same one the TPM reference has)
///
/// The Secure Enclave only does **NIST P-256**, not Ed25519, so this returns P-256 material: a
/// 65-byte x9.63 public key and a 64-byte ECDSA `r‖s` signature over `msg`. It therefore does
/// **not** yet plug into the Ed25519 `FfiWorkspace.createWithHardwareSigner` path — wiring a
/// Secure Enclave device in needs the P-256 hybrid-DSK variant tracked in `DEFERRED.md`. It is the
/// real hardware adapter + the on-device non-exportability check; for an end-to-end FFI round trip
/// in CI/local tests use ``SoftwareSigner`` (genuine Ed25519).
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
        try privateKey(keyAlias).signature(for: msg).rawRepresentation
    }

    public func assertNonExportable(keyAlias: String) throws {
        // The Secure Enclave exposes no API to read the private key, so possession of an
        // SE-resident key is itself the non-exportability guarantee. Confirm it is resident.
        _ = try privateKey(keyAlias)
    }
}
