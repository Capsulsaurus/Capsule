import Foundation

// MARK: - MockSalt

/// A field-scoped key for ``MockHash``.
///
/// Each derived field reads its own salt so the streams are independent: if
/// capture time and content type shared a stream, every video in the library
/// would land in the same week. The raw values are arbitrary odd constants —
/// only their distinctness matters.
public struct MockSalt: Sendable, Equatable, Hashable {
    public let rawValue: UInt64

    public init(rawValue: UInt64) {
        self.rawValue = rawValue
    }

    public static let identity = MockSalt(rawValue: 0x0101_0101_0101_0101)
    public static let dayWeight = MockSalt(rawValue: 0x1F3B_5D79_9BBD_DFF1)
    public static let timeOfDay = MockSalt(rawValue: 0x2A4C_6E80_A2C4_E607)
    public static let contentType = MockSalt(rawValue: 0x3B5D_7F91_B3D5_F719)
    public static let dimensions = MockSalt(rawValue: 0x4C6E_80A2_C4E6_0829)
    public static let orientation = MockSalt(rawValue: 0x5D7F_91B3_D5F7_193B)
    public static let camera = MockSalt(rawValue: 0x6E80_A2C4_E608_2A4D)
    public static let geolocation = MockSalt(rawValue: 0x7F91_B3D5_F719_3B5F)
    public static let rating = MockSalt(rawValue: 0x80A2_C4E6_082A_4C61)
    public static let cull = MockSalt(rawValue: 0x91B3_D5F7_193B_5D73)
    public static let userTags = MockSalt(rawValue: 0xA2C4_E608_2A4C_6E85)
    public static let aiTags = MockSalt(rawValue: 0xB3D5_F719_3B5D_7F97)
    public static let stacking = MockSalt(rawValue: 0xC4E6_082A_4C6E_80A9)
    public static let syncState = MockSalt(rawValue: 0xD5F7_193B_5D7F_91BB)
    public static let representation = MockSalt(rawValue: 0xE608_2A4C_6E80_A2CD)
    public static let caption = MockSalt(rawValue: 0xF719_3B5D_7F91_B3DF)
    public static let duration = MockSalt(rawValue: 0x082A_4C6E_80A2_C4F1)
    public static let colour = MockSalt(rawValue: 0x193B_5D7F_91B3_D503)
    public static let trip = MockSalt(rawValue: 0x2A4C_6E80_A2C4_E615)
    public static let people = MockSalt(rawValue: 0x3B5D_7F91_B3D5_F727)
    public static let schemaAhead = MockSalt(rawValue: 0x4C6E_80A2_C4E6_0839)
    public static let byteSize = MockSalt(rawValue: 0x5D7F_91B3_D5F7_194B)
}

// MARK: - MockHash

/// Deterministic derivation: a splitmix64 finalizer used as a **keyed hash**
/// rather than as a stateful generator.
///
/// A stateful PRNG would force the library to be produced in index order, which
/// is exactly what a paged, virtualized reader never does — it asks for index
/// 183 402 first and index 12 never. Keying on `(seed, index, salt)` makes every
/// field independently addressable in O(1), which is what lets ``MockLibrary``
/// be a pure function of `(seed, index)` and a 250 000-asset library cost
/// nothing to construct.
public enum MockHash {
    /// The splitmix64 finalizer (Steele, Lea & Flood). It avalanches every
    /// input bit, so two adjacent indices produce wholly uncorrelated fields —
    /// the property a hash-as-generator needs and a linear congruential step
    /// does not have.
    public static func mix(_ value: UInt64) -> UInt64 {
        var result = value &+ 0x9E37_79B9_7F4A_7C15
        result = (result ^ (result >> 30)) &* 0xBF58_476D_1CE4_E5B9
        result = (result ^ (result >> 27)) &* 0x94D0_49BB_1331_11EB
        return result ^ (result >> 31)
    }

    /// The hash for one field of one index.
    public static func value(seed: UInt64, index: Int, salt: MockSalt) -> UInt64 {
        mix(mix(seed ^ salt.rawValue) &+ UInt64(bitPattern: Int64(index)))
    }

    /// A second-order hash, for a field that varies within an already-derived
    /// grouping — a stack member's role, a tag's position in a table.
    public static func value(seed: UInt64, index: Int, salt: MockSalt, sub: Int) -> UInt64 {
        mix(value(seed: seed, index: index, salt: salt) &+ mix(UInt64(bitPattern: Int64(sub))))
    }

    // MARK: Projections

    /// A fraction in `[0, 1)`, from the high 53 bits so the low-bit structure of
    /// the multiplier never shows through.
    public static func fraction(_ hash: UInt64) -> Double {
        Double(hash >> 11) * (1.0 / 9007199254740992.0)
    }

    /// An integer in a closed range. Modulo bias is irrelevant at these range
    /// sizes and costs a division to avoid.
    public static func integer(_ hash: UInt64, in range: ClosedRange<Int>) -> Int {
        let span = range.upperBound - range.lowerBound + 1
        guard span > 0 else { return range.lowerBound }
        return range.lowerBound + Int(hash % UInt64(span))
    }

    /// Whether an event with the given per-mille probability fires.
    ///
    /// Per-mille rather than a `Double` because every scenario knob is written
    /// as a whole number of tenths of a percent, and integer comparison keeps
    /// the threshold exactly reproducible across architectures.
    public static func occurs(_ hash: UInt64, perMille: Int) -> Bool {
        guard perMille > 0 else { return false }
        guard perMille < 1000 else { return true }
        return Int(hash % 1000) < perMille
    }

    /// Pick from a table. Returns `nil` only for an empty table, so no call site
    /// needs a force-unwrap.
    public static func element<Value>(_ hash: UInt64, from table: [Value]) -> Value? {
        guard !table.isEmpty else { return nil }
        return table[Int(hash % UInt64(table.count))]
    }

    /// Pick from a **weighted** table, given the running weight total.
    ///
    /// Used for content types, where a uniform pick would give a library one
    /// third RAW files. Returns `nil` for an empty table.
    public static func weightedIndex(_ hash: UInt64, weights: [Int]) -> Int? {
        let total = weights.reduce(0, +)
        guard total > 0 else { return nil }
        var remaining = Int(hash % UInt64(total))
        for (position, weight) in weights.enumerated() {
            remaining -= weight
            if remaining < 0 { return position }
        }
        return weights.indices.last
    }

    /// Lowercase hex, zero-padded to `digits`.
    public static func hex(_ value: UInt64, digits: Int) -> String {
        let text = String(value, radix: 16)
        guard text.count < digits else { return String(text.suffix(digits)) }
        return String(repeating: "0", count: digits - text.count) + text
    }
}
