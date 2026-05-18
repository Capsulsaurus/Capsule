import Foundation
import ManagedStore

/// A deterministic, non-cryptographic ``ContentHasher`` for tests.
///
/// Identical bytes always hash the same and distinct bytes (effectively
/// always) differ — which is all the import pipeline's dedup logic needs,
/// without the cost of real SHA-256 in a unit test.
public struct MockContentHasher: ContentHasher {
    public init() {}

    public func hash(_ data: Data) -> String {
        Self.digest(of: data)
    }

    public func hashFile(at url: URL) async throws -> String {
        Self.digest(of: Data(url.absoluteString.utf8))
    }

    /// A 64-hex FNV-1a digest of the bytes.
    private static func digest(of data: Data) -> String {
        var value: UInt64 = 0xCBF2_9CE4_8422_2325
        for byte in data {
            value = (value ^ UInt64(byte)) &* 0x0000_0100_0000_01B3
        }
        let hex = String(value, radix: 16)
        return String((hex + String(repeating: "0", count: 64)).prefix(64))
    }
}
