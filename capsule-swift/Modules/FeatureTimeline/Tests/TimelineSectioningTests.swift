import Foundation
import Testing

import AssetKit
import CapsuleTestSupport
import FeatureTimeline

// MARK: - Duplicate section identifiers

/// `UICollectionViewDiffableDataSource` treats duplicate section identifiers as
/// a programmer error and raises `NSInternalInconsistencyException`, which
/// terminates the app. Bucketing therefore has to be duplicate-proof for *any*
/// input, not merely for the sorted input it is promised.
///
/// This is not hypothetical: it crashed the app on launch under the 250 000-asset
/// scenario, with `Duplicate identifiers: {("2026-07-12", "2026-07-11")}`.
@Suite("Timeline sectioning is duplicate-proof")
struct TimelineSectioningDuplicateTests {
    private static let calendar = Calendar(identifier: .gregorian)

    private static func asset(day: Int, hour: Int = 12) -> Asset {
        var components = DateComponents()
        components.year = 2026
        components.month = 7
        components.day = day
        components.hour = hour
        let date = calendar.date(from: components) ?? Date(timeIntervalSince1970: 0)
        return Fixtures.asset(id: .managed(uuid: "d\(day)-h\(hour)"), captureDate: date)
    }

    @Test("a day that appears in two non-adjacent runs yields one section")
    func interleavedDaysCoalesce() {
        // 12, 11, 12 — the shape that crashed: day 12 appears twice, split by 11.
        let assets = [Self.asset(day: 12, hour: 9), Self.asset(day: 11), Self.asset(day: 12, hour: 8)]
        let sections = TimelineSectioning.sections(from: assets, calendar: Self.calendar)

        let ids = sections.map { $0.id }
        #expect(ids == ["2026-07-12", "2026-07-11"])
        #expect(Set(ids).count == ids.count)
        // Nothing is dropped in the process.
        #expect(sections.map { $0.assets.count }.reduce(0, +) == assets.count)
    }

    @Test("section identifiers are unique for arbitrarily shuffled input")
    func shuffledInputHasUniqueIdentifiers() {
        // A deterministic scramble — no `shuffled()`, so a failure reproduces.
        let assets = (1 ... 20).map { Self.asset(day: ($0 * 7) % 12 + 1, hour: $0) }
        let sections = TimelineSectioning.sections(from: assets, calendar: Self.calendar)

        let ids = sections.map { $0.id }
        #expect(Set(ids).count == ids.count)
        #expect(sections.map { $0.assets.count }.reduce(0, +) == assets.count)
    }

    @Test("month and year aggregation are duplicate-proof too")
    func periodSectionsCoalesce() {
        var components = DateComponents()
        components.year = 2026
        let june = { (day: Int) -> Asset in
            components.month = 6
            components.day = day
            let date = Self.calendar.date(from: components) ?? Date(timeIntervalSince1970: 0)
            return Fixtures.asset(id: .managed(uuid: "jun-\(day)"), captureDate: date)
        }
        let july = { (day: Int) -> Asset in
            components.month = 7
            components.day = day
            let date = Self.calendar.date(from: components) ?? Date(timeIntervalSince1970: 0)
            return Fixtures.asset(id: .managed(uuid: "jul-\(day)"), captureDate: date)
        }

        let assets = [july(20), june(15), july(10)]
        let months = TimelineSectioning.monthSections(from: assets, calendar: Self.calendar)
        let years = TimelineSectioning.yearSections(from: assets, calendar: Self.calendar)

        #expect(months.map { $0.id } == ["2026-07", "2026-06"])
        #expect(Set(months.map { $0.id }).count == months.count)
        #expect(years.map { $0.id } == ["2026"])
    }

    /// A sorted input — the normal case — must be unchanged by the fix.
    @Test("correctly ordered input keeps its order")
    func sortedInputIsUnaffected() {
        let assets = [Self.asset(day: 12), Self.asset(day: 11), Self.asset(day: 10)]
        let sections = TimelineSectioning.sections(from: assets, calendar: Self.calendar)
        #expect(sections.map { $0.id } == ["2026-07-12", "2026-07-11", "2026-07-10"])
    }
}
