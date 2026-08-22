import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - MockIdentifiers

/// Deterministic identifier minting for everything that is not an asset.
///
/// **No `UUID()` anywhere.** A mock that minted random identifiers would give a
/// different album id on every launch, so a UI test could not name a row, a
/// snapshot could not be compared, and "same seed, same world" would be false.
/// Every id here is a pure function of `(seed, kind, ordinal)`, shaped like the
/// real thing — UUIDv7 for the time-ordered kinds, UUIDv4 for the ones whose
/// creation time must not leak, unstructured hex for a share link's opaque id.
public enum MockIdentifiers {
    /// Namespaces, so an album ordinal 3 and a person ordinal 3 never collide.
    private enum Namespace: UInt64 {
        case album = 1
        case device = 2
        case session = 3
        case person = 4
        case stack = 5
        case share = 6
        case drop = 7
        case quarantine = 8
        case smartAlbum = 9
        case upload = 10
        case albumGroup = 11
        case blob = 12
        case importRun = 13
        case peer = 14
    }

    /// A UUIDv7-shaped string: a monotonic-looking prefix plus derived entropy.
    /// Used where the real system mints a v7.
    private static func timeOrdered(_ seed: UInt64, _ namespace: Namespace, _ ordinal: Int) -> String {
        uuidText(seed: seed, namespace: namespace, ordinal: ordinal, version: 0x7000)
    }

    /// A UUIDv4-shaped string, for the kinds whose creation order must not leak.
    private static func unordered(_ seed: UInt64, _ namespace: Namespace, _ ordinal: Int) -> String {
        uuidText(seed: seed, namespace: namespace, ordinal: ordinal, version: 0x4000)
    }

    private static func uuidText(seed: UInt64, namespace: Namespace, ordinal: Int, version: UInt64) -> String {
        let base = MockHash.mix(seed ^ (namespace.rawValue &* 0x9E37_79B9_7F4A_7C15))
        let first = MockHash.mix(base &+ UInt64(bitPattern: Int64(ordinal)))
        let second = MockHash.mix(first)
        return [
            MockHash.hex(first >> 32, digits: 8),
            MockHash.hex((first >> 16) & 0xFFFF, digits: 4),
            MockHash.hex((first & 0x0FFF) | version, digits: 4),
            MockHash.hex((second >> 48) & 0x3FFF | 0x8000, digits: 4),
            MockHash.hex(second & 0xFFFF_FFFF_FFFF, digits: 12),
        ].joined(separator: "-")
    }

    // MARK: Kinds

    public static func albumID(seed: UInt64, ordinal: Int) -> AlbumID {
        .managed(uuid: timeOrdered(seed, .album, ordinal))
    }

    public static func deviceID(seed: UInt64, ordinal: Int) -> DeviceID {
        DeviceID(unordered(seed, .device, ordinal))
    }

    public static func sessionID(seed: UInt64, ordinal: Int) -> SessionID {
        SessionID(timeOrdered(seed, .session, ordinal))
    }

    public static func personID(seed: UInt64, ordinal: Int) -> PersonID {
        PersonID(timeOrdered(seed, .person, ordinal))
    }

    public static func stackID(seed: UInt64, ordinal: Int) -> StackID {
        StackID(timeOrdered(seed, .stack, ordinal))
    }

    public static func shareID(seed: UInt64, ordinal: Int) -> ShareID {
        ShareID(timeOrdered(seed, .share, ordinal))
    }

    public static func dropID(seed: UInt64, ordinal: Int) -> DropID {
        DropID(timeOrdered(seed, .drop, ordinal))
    }

    public static func quarantineID(seed: UInt64, ordinal: Int) -> QuarantineID {
        QuarantineID(timeOrdered(seed, .quarantine, ordinal))
    }

    public static func smartAlbumID(seed: UInt64, ordinal: Int) -> SmartAlbumID {
        SmartAlbumID(timeOrdered(seed, .smartAlbum, ordinal))
    }

    public static func uploadID(seed: UInt64, ordinal: Int) -> UploadID {
        UploadID(timeOrdered(seed, .upload, ordinal))
    }

    public static func albumGroupID(seed: UInt64, ordinal: Int) -> AlbumGroupID {
        AlbumGroupID(timeOrdered(seed, .albumGroup, ordinal))
    }

    public static func importID(seed: UInt64, ordinal: Int) -> ImportID {
        ImportID(timeOrdered(seed, .importRun, ordinal))
    }

    public static func peerID(origin: String) -> PeerID {
        PeerID(origin)
    }

    /// A content address: lowercase hex, length fixed by the crypto suite (32
    /// bytes here, matching a SHA-256 digest).
    public static func blobHash(seed: UInt64, ordinal: Int) -> BlobHash {
        let first = MockHash.mix(seed ^ (Namespace.blob.rawValue &* 0x9E37_79B9))
        let parts = (0 ..< 4).map { step in
            MockHash.hex(MockHash.mix(first &+ UInt64(bitPattern: Int64(ordinal &* 4 &+ step))), digits: 16)
        }
        return BlobHash(parts.joined())
    }

    /// A share link's URL path component — 128 bits of hex, deliberately
    /// **not** a UUIDv7, because a structured id in a URL leaks creation order.
    public static func opaqueLinkID(seed: UInt64, ordinal: Int) -> String {
        let first = MockHash.mix(seed &+ UInt64(bitPattern: Int64(ordinal)) &+ 0xA11C)
        return MockHash.hex(first, digits: 16) + MockHash.hex(MockHash.mix(first), digits: 16)
    }

    /// A share link's fragment secret. Never sent to a server, and in a real
    /// build never logged; here it is derived so a test can assert on it.
    public static func linkSecret(seed: UInt64, ordinal: Int) -> String {
        let first = MockHash.mix(seed &+ UInt64(bitPattern: Int64(ordinal)) &+ 0x5EC2)
        return MockHash.hex(first, digits: 16) + MockHash.hex(MockHash.mix(first), digits: 16)
    }

    /// The advisory device-cohort hash. Advisory by design: no authorization
    /// decision may read it, so the mock's value is as good as a real one.
    public static func cohortHash(seed: UInt64, ordinal: Int) -> String {
        MockHash.hex(MockHash.mix(seed &+ 0xC01234 &+ UInt64(bitPattern: Int64(ordinal))), digits: 16)
    }
}
