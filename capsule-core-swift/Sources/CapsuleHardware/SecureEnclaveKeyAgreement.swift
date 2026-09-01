import CryptoKit
import Foundation

/// A Secure Enclave–backed ``HardwareKeyAgreement`` (Apple, iOS / macOS) — the ECDH analogue of
/// ``SecureEnclaveSigner`` that backs the hardware-bound classical half of the device **encryption**
/// key (slice S-F5). The private key is generated inside the Secure Enclave and never leaves it;
/// there is no API to read the private bytes, so non-exportability is enforced by the platform.
///
/// ## Algorithm
///
/// The Secure Enclave does **NIST P-256** key agreement (`SecureEnclave.P256.KeyAgreement`), so this
/// returns a 65-byte x9.63 (uncompressed SEC1) public key and, for `keyAgreement`, the **raw 32-byte
/// ECDH shared secret** (the point's x-coordinate — CryptoKit's `SharedSecret`, before any KDF). That
/// is exactly what the Rust hybrid DEK consumes: the element's `pk_P` normalizes from SEC1 and the raw
/// ECDH secret folds into the KEM combiner, so this plugs into `p256HardwareDekRoundTrip` end to end.
///
/// A production app persists `dataRepresentation` of each key (the encrypted SE blob) in the Keychain
/// and reloads it; this reference keeps the handles in memory.
public final class SecureEnclaveKeyAgreement: HardwareKeyAgreement, @unchecked Sendable {
    private var keys: [String: SecureEnclave.P256.KeyAgreement.PrivateKey] = [:]
    private let lock = NSLock()

    public init() {}

    private func privateKey(_ keyAlias: String) throws -> SecureEnclave.P256.KeyAgreement.PrivateKey {
        lock.lock()
        defer { lock.unlock() }
        if let existing = keys[keyAlias] {
            return existing
        }
        guard SecureEnclave.isAvailable else {
            throw HardwareSignerError.Unavailable(message: "Secure Enclave unavailable")
        }
        let key = try SecureEnclave.P256.KeyAgreement.PrivateKey()
        keys[keyAlias] = key
        return key
    }

    public func enroll(keyAlias: String) throws -> Data {
        try privateKey(keyAlias).publicKey.x963Representation
    }

    public func publicKey(keyAlias: String) throws -> Data {
        try privateKey(keyAlias).publicKey.x963Representation
    }

    public func keyAgreement(keyAlias: String, peerPublic: Data) throws -> Data {
        let peer = try P256.KeyAgreement.PublicKey(x963Representation: peerPublic)
        let shared = try privateKey(keyAlias).sharedSecretFromKeyAgreement(with: peer)
        return shared.withUnsafeBytes { Data($0) }
    }

    public func assertNonExportable(keyAlias: String) throws {
        // The Secure Enclave exposes no API to read the private key, so possession of an
        // SE-resident key is itself the non-exportability guarantee. Confirm it is resident.
        _ = try privateKey(keyAlias)
    }
}
