import CryptoKit
import Foundation

/// Computes the content hash that identifies a media file.
///
/// Capsule keys every asset by the **SHA-256** of its file bytes: it is the
/// import pipeline's dedup key and the sidecar's integrity check. Hashing sits
/// behind a protocol so the import pipeline can be tested against a mock that
/// returns deterministic digests without touching a real file.
public protocol ContentHasher: Sendable {
    /// The lowercase-hex SHA-256 of the file at `url`, read in bounded chunks
    /// so even a multi-gigabyte video never loads wholly into memory.
    func hashFile(at url: URL) async throws -> String

    /// The lowercase-hex SHA-256 of an in-memory byte buffer.
    func hash(_ data: Data) -> String
}

/// The production ``ContentHasher`` — `CryptoKit`'s `SHA256`, which uses the
/// CPU's hardware SHA-2 instructions (ARMv8 Crypto Extensions on every
/// supported iPhone).
public struct CryptoKitHasher: ContentHasher {
    /// Bytes read per chunk while streaming a file (1 MiB).
    private static let chunkSize = 1 << 20

    public init() {}

    public func hash(_ data: Data) -> String {
        SHA256.hash(data: data).hexString
    }

    public func hashFile(at url: URL) async throws -> String {
        let handle = try FileHandle(forReadingFrom: url)
        defer { try? handle.close() }

        var hasher = SHA256()
        while let chunk = try handle.read(upToCount: Self.chunkSize), !chunk.isEmpty {
            hasher.update(data: chunk)
        }
        return hasher.finalize().hexString
    }
}

extension SHA256Digest {
    /// The digest as a 64-character lowercase hex string — the canonical form
    /// stored in the catalog and sidecars.
    var hexString: String {
        var output = ""
        output.reserveCapacity(SHA256Digest.byteCount * 2)
        for byte in self {
            output += String(byte >> 4, radix: 16)
            output += String(byte & 0x0F, radix: 16)
        }
        return output
    }
}
