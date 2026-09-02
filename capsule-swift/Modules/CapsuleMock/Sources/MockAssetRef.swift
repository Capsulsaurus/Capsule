import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - MockAssetRef

/// The coordinates of a derived asset, encoded into — and recoverable from —
/// its ``AssetID``.
///
/// Every port takes an `AssetID` and has to answer questions about it. With a
/// materialized array that is a dictionary lookup; with a *derived* library
/// there is nothing to look up in, so the identifier itself has to carry enough
/// to re-derive the asset. The last group of the UUID is therefore structured:
///
/// ```text
/// xxxxxxxx-xxxx-xxxx-xxxx-KKMMIIIIIIII
///                         │ │ └── base index, big-endian u32
///                         │ └──── member ordinal within a stack
///                         └────── kind
/// ```
///
/// The first ten bytes stay hash-derived, so identifiers still look and sort
/// like opaque UUIDs and no test can accidentally depend on their text. This is
/// the one place the mock trades opacity for O(1) resolution, and it buys the
/// whole 250 000-asset story: `asset(for:)` is a decode plus a derivation, not a
/// scan.
public struct MockAssetRef: Sendable, Equatable, Hashable {
    /// Which derived population the asset belongs to.
    ///
    /// The populations are kept apart because the default timeline must be
    /// exactly the base population — that is what makes an unfiltered
    /// `dayCounts(...)` an O(days) read instead of an O(assets) filter. Trashed,
    /// user-hidden, and collapsed stack members are all *outside* the default
    /// timeline by construction, so they live in their own populations rather
    /// than being filtered out of the main one.
    public enum Kind: UInt8, Sendable, Equatable, Hashable, CaseIterable {
        /// In the default timeline. The base population.
        case live = 0x00
        /// A non-cover member of a collapsed stack.
        case stackMember = 0x01
        /// Soft-deleted, in the retention window.
        case trashed = 0x10
        /// User-hidden.
        case userHidden = 0x11
    }

    public var kind: Kind
    /// Index within the population — for ``Kind/stackMember``, the index of the
    /// stack's primary.
    public var index: Int
    /// Position within the stack, 1-based. Zero for every other kind.
    public var memberOrdinal: UInt8

    public init(kind: Kind, index: Int, memberOrdinal: UInt8 = 0) {
        self.kind = kind
        self.index = index
        self.memberOrdinal = memberOrdinal
    }

    /// The derivation index — the value fed to ``MockHash`` for this asset's
    /// fields.
    ///
    /// Populations are offset far apart so a trashed asset and a live one at the
    /// same ordinal derive completely different content, and a stack member
    /// derives differently from its own primary.
    public var derivationIndex: Int {
        switch kind {
        case .live: index
        case .stackMember: 0x2000_0000 &+ index &* 8 &+ Int(memberOrdinal)
        case .trashed: 0x4000_0000 &+ index
        case .userHidden: 0x6000_0000 &+ index
        }
    }
}

// MARK: - Identifier coding

public extension MockAssetRef {
    /// The asset identifier for these coordinates.
    ///
    /// - Parameter seed: the world seed, so two scenarios with different seeds
    ///   never produce colliding identifiers.
    func identifier(seed: UInt64) -> AssetID {
        .managed(uuid: uuidString(seed: seed))
    }

    /// The UUID text, with the version and variant nibbles set so the string is
    /// a structurally valid UUIDv7 and not merely UUID-shaped.
    func uuidString(seed: UInt64) -> String {
        let entropy = MockHash.value(seed: seed, index: derivationIndex, salt: .identity)
        let high = MockHash.hex(entropy >> 24, digits: 8)
        let midOne = MockHash.hex((entropy >> 12) & 0xFFF | 0x7000, digits: 4)
        let midTwo = MockHash.hex(entropy & 0xFFF | 0x8000, digits: 4)
        let variant = MockHash.hex(MockHash.mix(entropy) & 0x3FFF | 0x8000, digits: 4)
        let tail = MockHash.hex(UInt64(kind.rawValue), digits: 2)
            + MockHash.hex(UInt64(memberOrdinal), digits: 2)
            + MockHash.hex(UInt64(UInt32(truncatingIfNeeded: index)), digits: 8)
        return "\(high)-\(midOne)-\(midTwo)-\(variant)-\(tail)"
    }

    /// Recover the coordinates from an identifier, or `nil` when the identifier
    /// did not come from this mock.
    ///
    /// Returning `nil` rather than a default matters: a port asked about an
    /// unknown asset must answer "no such asset", and silently coercing a
    /// PhotoKit identifier into index 0 would make every miss look like a hit on
    /// the newest photo.
    static func decode(_ identifier: AssetID) -> MockAssetRef? {
        guard case let .managed(uuid) = identifier else { return nil }
        let groups = uuid.split(separator: "-")
        guard groups.count == 5, let tail = groups.last, tail.count == 12 else { return nil }
        let digits = Array(tail)
        guard let kindRaw = UInt8(String(digits[0 ..< 2]), radix: 16),
              let kind = Kind(rawValue: kindRaw),
              let ordinal = UInt8(String(digits[2 ..< 4]), radix: 16),
              let index = UInt32(String(digits[4 ..< 12]), radix: 16)
        else { return nil }
        return MockAssetRef(kind: kind, index: Int(index), memberOrdinal: ordinal)
    }
}
