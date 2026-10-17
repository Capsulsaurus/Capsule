import Foundation

// MARK: - Identifier

/// The shared shape of every Capsule identifier: an opaque, canonical string.
///
/// The identifiers below deliberately wrap `String` rather than `UUID`. Two
/// reasons, both from *Metadata — Identifiers*: the wire form is the canonical
/// lowercase-hyphenated text (a `UUID` round-trip could re-case it and break a
/// byte-for-byte comparison), and not every identifier is a UUID — a share
/// link's opaque id is 128 random bits in hex, deliberately *not* structured.
///
/// Whether a given id is UUIDv7 (time-ordered) or UUIDv4 (creation time must
/// not leak) is documented per type; it is a property of the minting side, and
/// this layer never re-mints.
public protocol CapsuleIdentifier: Sendable, Hashable, Codable, CustomStringConvertible, Comparable {
    /// The canonical string form, verbatim as it crossed the boundary.
    var rawValue: String { get }
    init(_ rawValue: String)
}

public extension CapsuleIdentifier {
    var description: String { rawValue }

    static func < (lhs: Self, rhs: Self) -> Bool {
        lhs.rawValue < rhs.rawValue
    }
}

// MARK: - Identifiers

/// A person cluster produced by on-device face grouping (*AI — AI Output
/// Containment*). Cluster identity is AI-derived and model-scoped: a cluster id
/// is only comparable within one `(model_id, model_version)` slot.
public struct PersonID: CapsuleIdentifier {
    public var rawValue: String
    public init(_ rawValue: String) { self.rawValue = rawValue }
}

/// A stack of related assets — a RAW+JPEG pair, a burst, a Live Photo
/// (*Asset Organization — Asset Stacking*). UUIDv7.
public struct StackID: CapsuleIdentifier {
    public var rawValue: String
    public init(_ rawValue: String) { self.rawValue = rawValue }
}

/// An issued share link, for revocation (*Share Links*).
///
/// This is the **owner-held revocation handle** (UUIDv7), never the URL's
/// opaque id — see ``ShareLink/opaqueID``, which must stay unstructured so the
/// URL leaks no creation ordering.
public struct ShareID: CapsuleIdentifier {
    public var rawValue: String
    public init(_ rawValue: String) { self.rawValue = rawValue }
}

/// A pending web-upload drop in the provisioning user's inbox (*Web Upload —
/// Drop and Adoption Lifecycle*).
public struct DropID: CapsuleIdentifier {
    public var rawValue: String
    public init(_ rawValue: String) { self.rawValue = rawValue }
}

/// An enrolled device (*Cryptography — Keys: Device Directory*).
///
/// **UUIDv4, not v7** — a device id must not leak creation ordering. It is also
/// the lexicographic tiebreaker for every LWW register, so its exact string
/// form is load-bearing (see ``Lww``).
public struct DeviceID: CapsuleIdentifier {
    public var rawValue: String
    public init(_ rawValue: String) { self.rawValue = rawValue }
}

/// An authenticated session (*Authentication — Session ID*). UUIDv7, minted
/// server-side on successful authentication. Never a token: the session
/// *secret* stays inside the SDK and never reaches this layer.
public struct SessionID: CapsuleIdentifier {
    public var rawValue: String
    public init(_ rawValue: String) { self.rawValue = rawValue }
}

/// One entry in the quarantine inventory (*Threat Model — Quarantine
/// Surfaces*). Client-local: quarantine is a client surface, so the id is
/// minted on the device that quarantined the item.
public struct QuarantineID: CapsuleIdentifier {
    public var rawValue: String
    public init(_ rawValue: String) { self.rawValue = rawValue }
}

/// A user-defined smart album (*Asset Organization — Smart-Album Definition
/// Schema*). UUIDv7; the key of one LWW register in the library-settings
/// document, so a stamped deletion tombstone reuses the same id.
public struct SmartAlbumID: CapsuleIdentifier {
    public var rawValue: String
    public init(_ rawValue: String) { self.rawValue = rawValue }
}

/// One import run (*Import — Pipeline*). UUIDv7, so a run's identifier sorts
/// chronologically in a progress log.
public struct ImportID: CapsuleIdentifier {
    public var rawValue: String
    public init(_ rawValue: String) { self.rawValue = rawValue }
}

/// A federated peer server (*Federation*). The identity is the peer's canonical
/// origin; every containment budget, circuit breaker, and moderation decision is
/// scoped to it, so the id is the blast-radius boundary.
public struct PeerID: CapsuleIdentifier {
    public var rawValue: String
    public init(_ rawValue: String) { self.rawValue = rawValue }
}

/// An album group — the client-side identity of an aggregated federated album
/// (*Federation — The Album-Group Assertion*). UUIDv7, minted by the creator and
/// shared in the invite; no server ever learns a group exists.
public struct AlbumGroupID: CapsuleIdentifier {
    public var rawValue: String
    public init(_ rawValue: String) { self.rawValue = rawValue }
}

/// A content address: the lowercase-hex digest of a **ciphertext** blob
/// (*Cryptography — Primitives*). Digest length is fixed by `crypto_suite_id`,
/// so this is a string rather than a fixed-width type.
public struct BlobHash: CapsuleIdentifier {
    public var rawValue: String
    public init(_ rawValue: String) { self.rawValue = rawValue }
}

/// An upload session (*Upload Protocol — Endpoints*). The `{id}` in
/// `/upload/{id}`, and the key every custody receipt is issued against.
public struct UploadID: CapsuleIdentifier {
    public var rawValue: String
    public init(_ rawValue: String) { self.rawValue = rawValue }
}
