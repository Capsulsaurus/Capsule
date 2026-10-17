import CapsuleDomain
import CapsuleFoundation
import Foundation
import Testing

@testable import CapsuleMock

// MARK: - Determinism

/// The property everything else rests on: the library is a **pure function of
/// `(seed, index)`**.
///
/// If it were not, every other guarantee would be conditional. A UI test could
/// not name a row, a snapshot could not be compared, and "the same scenario"
/// would mean "a similar scenario". These tests check it the only way that
/// matters — across two separately constructed instances, not just twice on one.
@Suite("Derivation is deterministic")
struct MockDeterminismTests {
    private static let probeIndices = [0, 1, 2, 17, 199, 1000, 3999]

    private func makeProfile(seed: UInt64 = 0xABCD_1234_5678_9ABC, assetCount: Int = 4000) -> MockLibraryProfile {
        MockLibraryProfile(
            seed: seed,
            assetCount: assetCount,
            newestDayNumber: MockClock.reference.todayDayNumber
        )
    }

    @Test("The same index yields an identical asset twice")
    func repeatedDerivationIsIdentical() {
        let library = MockLibrary(profile: makeProfile())
        for index in Self.probeIndices {
            #expect(library.asset(at: index) == library.asset(at: index))
        }
    }

    @Test("Two separately constructed libraries agree asset for asset")
    func separateInstancesAgree() {
        let profile = makeProfile()
        let first = MockLibrary(profile: profile)
        let second = MockLibrary(profile: profile)
        #expect(first.unfilteredDayCounts() == second.unfilteredDayCounts())
        for index in Self.probeIndices {
            #expect(first.asset(at: index) == second.asset(at: index))
            #expect(first.facets(at: index) == second.facets(at: index))
        }
    }

    @Test("A different seed produces a different library")
    func seedChangesTheWorld() {
        let first = MockLibrary(profile: makeProfile(seed: 1))
        let second = MockLibrary(profile: makeProfile(seed: 2))
        let differences = Self.probeIndices.filter { first.asset(at: $0) != second.asset(at: $0) }
        #expect(differences.count == Self.probeIndices.count)
    }

    /// The identifier carries the coordinates it was derived from, which is what
    /// makes `asset(for:)` a decode rather than a scan.
    @Test("Identifiers round-trip through their encoded coordinates")
    func identifiersRoundTrip() {
        let library = MockLibrary(profile: makeProfile())
        for index in Self.probeIndices {
            let ref = MockAssetRef(kind: .live, index: index)
            let decoded = MockAssetRef.decode(ref.identifier(seed: library.profile.seed))
            #expect(decoded == ref)
        }
        let member = MockAssetRef(kind: .stackMember, index: 42, memberOrdinal: 3)
        #expect(MockAssetRef.decode(member.identifier(seed: 7)) == member)
    }

    /// A PhotoKit identifier is not this library's, and answering for it would
    /// turn every miss into a hit on the newest photo.
    @Test("A foreign identifier decodes to nothing")
    func foreignIdentifiersAreRejected() {
        #expect(MockAssetRef.decode(.photoKit(localIdentifier: "ABC-123")) == nil)
        #expect(MockAssetRef.decode(.managed(uuid: "not-a-uuid")) == nil)
    }

    /// Index order and the domain's own comparator must be the same order, or a
    /// grid's offsets stop meaning anything.
    @Test("Derived order matches the domain's newest-first comparator")
    func derivedOrderMatchesComparator() {
        let library = MockLibrary(profile: makeProfile())
        for index in 0 ..< 400 {
            let earlier = library.asset(at: index)
            let later = library.asset(at: index + 1)
            #expect(LibraryAsset.isOrderedNewestFirst(earlier, later))
        }
    }

    /// The day boundaries partition the index space exactly: every asset falls
    /// on the day whose range contains it, and the counts sum to the library.
    @Test("Day boundaries partition the index space")
    func dayBoundariesPartitionTheIndex() {
        let library = MockLibrary(profile: makeProfile())
        let counts = library.unfilteredDayCounts()
        #expect(counts.totalCount == library.assetCount)
        #expect(counts.allSatisfy { $0.count >= 1 })
        for index in stride(from: 0, to: library.assetCount, by: 37) {
            let dayIndex = library.dayIndex(forAsset: index)
            #expect(library.indexRange(forDay: dayIndex).contains(index))
            #expect(library.asset(at: index).dayKey == library.dayKey(forDay: dayIndex))
        }
    }

    /// Sections are ordered oldest first, as ``LibraryPort/dayCounts(matching:)``
    /// specifies, and lexicographic day order is chronological order.
    @Test("Day counts are oldest day first")
    func dayCountsAreOldestFirst() {
        let counts = MockLibrary(profile: makeProfile()).unfilteredDayCounts()
        #expect(counts == counts.sorted { $0.dayKey < $1.dayKey })
    }
}
