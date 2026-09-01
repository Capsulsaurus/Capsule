import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - MockPeople

/// Face-cluster membership, defined so it is **invertible**.
///
/// A cluster could be defined by hashing each asset — but then listing a
/// cluster's assets would mean scanning the whole library, which at 250 000
/// assets makes the People surface the one screen that cannot be paged. So
/// membership is arithmetic instead: cluster *k* owns every index congruent to
/// its residue modulo its stride. The *n*-th member is then `residue + n·stride`
/// in O(1), the count is a division, and a person's photos page exactly like the
/// timeline does.
///
/// An asset can belong to several clusters, which is both realistic and the case
/// a "who is in this photo" row has to handle.
public enum MockPeople {
    /// How many clusters the world has. Enough that the People grid scrolls;
    /// few enough that each has a meaningful number of photographs.
    public static let clusterCount = 14

    /// Names are user data — a cluster is unnamed until somebody names it, and
    /// a fabricated placeholder would be worse than nothing. Roughly half start
    /// named, so both states are on screen at once.
    static let names = ["avery", "morgan", "sam", "riley", "jordan", "kai", "noor"]

    /// The stride and residue that define one cluster's membership.
    ///
    /// Strides are drawn from a spread of values so cluster sizes differ by an
    /// order of magnitude, which is what a real library looks like: a partner
    /// appears in thousands of photographs and a colleague in nine.
    public static func membership(seed: UInt64, ordinal: Int) -> (stride: Int, residue: Int) {
        let hash = MockHash.value(seed: seed, index: ordinal, salt: .people)
        let strides = [23, 31, 47, 61, 89, 127, 199, 307, 419, 613]
        let stride = MockHash.element(hash, from: strides) ?? 61
        return (stride, MockHash.integer(MockHash.mix(hash), in: 0 ... (stride - 1)))
    }

    /// How many assets a cluster holds.
    public static func assetCount(seed: UInt64, ordinal: Int, libraryCount: Int) -> Int {
        let rule = membership(seed: seed, ordinal: ordinal)
        guard libraryCount > rule.residue else { return 0 }
        return (libraryCount - rule.residue + rule.stride - 1) / rule.stride
    }

    /// The library index of a cluster's *n*-th member, newest first.
    public static func memberIndex(seed: UInt64, ordinal: Int, position: Int) -> Int {
        let rule = membership(seed: seed, ordinal: ordinal)
        return rule.residue + position * rule.stride
    }

    /// Which clusters an asset belongs to.
    public static func clusters(seed: UInt64, containing index: Int) -> [Int] {
        (0 ..< clusterCount).filter { ordinal in
            let rule = membership(seed: seed, ordinal: ordinal)
            return index % rule.stride == rule.residue
        }
    }

    /// A cluster's derived name, or `nil` for one nobody has named.
    public static func derivedName(seed: UInt64, ordinal: Int) -> String? {
        let hash = MockHash.value(seed: seed, index: ordinal, salt: .people, sub: 2)
        guard MockHash.occurs(hash, perMille: 520) else { return nil }
        return MockHash.element(MockHash.mix(hash), from: names)
    }

    /// Whether a cluster is **stale** — its slot's canonical model changed, so
    /// it is excluded from evaluation until regenerated rather than compared
    /// across model versions.
    ///
    /// Included in listings rather than hidden: silently dropping a named person
    /// is worse than showing them as pending.
    public static func isStale(seed: UInt64, ordinal: Int) -> Bool {
        MockHash.occurs(MockHash.value(seed: seed, index: ordinal, salt: .people, sub: 3), perMille: 160)
    }

    public static func isHidden(seed: UInt64, ordinal: Int) -> Bool {
        MockHash.occurs(MockHash.value(seed: seed, index: ordinal, salt: .people, sub: 4), perMille: 80)
    }
}
