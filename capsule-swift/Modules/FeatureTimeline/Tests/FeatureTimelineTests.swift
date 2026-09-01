import Foundation
import Testing

import AssetKit
import CapsuleTestSupport
import FeatureTimeline

@Suite("TimelineSectioning day grouping")
struct TimelineSectioningTests {
    private var utcCalendar: Calendar {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = .gmt
        return calendar
    }

    @Test("an empty asset list yields no sections")
    func emptyInput() {
        #expect(TimelineSectioning.sections(from: []).isEmpty)
    }

    @Test("assets are bucketed into one section per capture day")
    func groupsByDay() {
        // 2024-07-03 12:26:40 UTC and ~27h later, 2024-07-04.
        let dayOne = Date(timeIntervalSince1970: 1720000000)
        let dayTwo = Date(timeIntervalSince1970: 1720100000)
        let assets = [
            Fixtures.asset(captureDate: dayTwo.addingTimeInterval(120)),
            Fixtures.asset(captureDate: dayTwo),
            Fixtures.asset(captureDate: dayOne),
        ]
        let sections = TimelineSectioning.sections(
            from: assets,
            calendar: utcCalendar,
            referenceDate: dayTwo
        )
        #expect(sections.count == 2)
        #expect(sections[0].assets.count == 2)
        #expect(sections[1].assets.count == 1)
        #expect(sections[0].id != sections[1].id)
    }

    /// The titles are catalog lookups, not literals, so the expectation resolves
    /// the same keys the implementation does — otherwise this would assert the
    /// English catalog rather than the sectioning, and fail in a test bundle that
    /// carries no string catalog at all.
    @Test("the current and prior days are titled Today and Yesterday")
    func relativeDayTitles() {
        let today = Date(timeIntervalSince1970: 1720000000)
        let yesterday = today.addingTimeInterval(-86400)
        let sections = TimelineSectioning.sections(
            from: [Fixtures.asset(captureDate: today), Fixtures.asset(captureDate: yesterday)],
            calendar: utcCalendar,
            referenceDate: today
        )
        #expect(sections.first?.title == String(localized: "app.timeline.section.today"))
        #expect(sections.last?.title == String(localized: "app.timeline.section.yesterday"))
        #expect(sections.first?.title != sections.last?.title)
    }

    @Test("same-day assets collapse into a single section")
    func sameDayCollapses() {
        let base = Date(timeIntervalSince1970: 1720000000)
        let assets = (0 ..< 4).map { Fixtures.asset(captureDate: base.addingTimeInterval(Double($0) * 600)) }
        let sections = TimelineSectioning.sections(from: assets, calendar: utcCalendar, referenceDate: base)
        #expect(sections.count == 1)
        #expect(sections[0].assets.count == 4)
    }
}

@Suite("TimelineSectioning aggregation levels")
struct TimelineAggregationTests {
    private var utcCalendar: Calendar {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = .gmt
        return calendar
    }

    private struct Fixture {
        let assets: [Asset]
        let julyNewest: Asset
        let dec2023: Asset
    }

    // 2024-07-15, 2024-07-01, 2024-06-20, 2023-12-25 (UTC), newest first.
    private func makeFixture() -> Fixture {
        let julyA = Fixtures.asset(captureDate: Date(timeIntervalSince1970: 1721001600))
        let julyB = Fixtures.asset(captureDate: Date(timeIntervalSince1970: 1719792000))
        let june = Fixtures.asset(captureDate: Date(timeIntervalSince1970: 1718841600))
        let dec = Fixtures.asset(captureDate: Date(timeIntervalSince1970: 1703462400))
        return Fixture(assets: [julyA, julyB, june, dec], julyNewest: julyA, dec2023: dec)
    }

    @Test("months bucket into one representative section each, newest first")
    func monthSections() {
        let fixture = makeFixture()
        let sections = TimelineSectioning.monthSections(from: fixture.assets, calendar: utcCalendar)
        #expect(sections.count == 3)
        #expect(sections[0].id == "2024-07")
        #expect(sections[0].title == "July 2024")
        #expect(sections[0].assets == [fixture.julyNewest]) // newest of the month represents it
        #expect(sections.map(\.id) == ["2024-07", "2024-06", "2023-12"])
    }

    @Test("years bucket into one representative section each, newest first")
    func yearSections() {
        let fixture = makeFixture()
        let sections = TimelineSectioning.yearSections(from: fixture.assets, calendar: utcCalendar)
        #expect(sections.count == 2)
        #expect(sections[0].id == "2024")
        #expect(sections[0].title == "2024")
        #expect(sections.last?.id == "2023")
        #expect(sections.last?.assets == [fixture.dec2023])
    }

    @Test("aggregation ids nest by prefix, so drill-down can focus a period")
    func idsNestByPrefix() {
        let fixture = makeFixture()
        let years = TimelineSectioning.yearSections(from: fixture.assets, calendar: utcCalendar)
        let months = TimelineSectioning.monthSections(from: fixture.assets, calendar: utcCalendar)
        let days = TimelineSectioning.sections(from: fixture.assets, calendar: utcCalendar)
        // A month id is prefixed by its year id; a day id by its month id.
        #expect(months.contains { $0.id.hasPrefix(years[0].id) })
        #expect(days.contains { $0.id.hasPrefix(months[0].id) })
    }

    @Test("empty input yields no aggregation sections")
    func emptyInput() {
        #expect(TimelineSectioning.monthSections(from: []).isEmpty)
        #expect(TimelineSectioning.yearSections(from: []).isEmpty)
    }
}

@Suite("TimelineViewModel")
@MainActor
struct TimelineViewModelTests {
    @Test("loads an authorized library as one uninterrupted run")
    func loadsAuthorizedLibrary() async {
        let provider = MockAssetProvider(assets: Fixtures.assets(count: 6), status: .authorized)
        let model = TimelineViewModel(provider: provider)
        await model.load()
        #expect(model.state == .ready)
        // Six assets a day apart used to be six day sections. All Photos is now
        // one continuous field of tiles, so they are one section of six — the
        // days are still visible in the dates, not in the layout.
        #expect(model.sections.count == 1)
        #expect(model.sections.first?.assets.count == 6)
    }

    @Test("surfaces the permission prompt when access is denied")
    func deniedAccess() async {
        let provider = MockAssetProvider(assets: [], status: .denied)
        let model = TimelineViewModel(provider: provider)
        await model.load()
        #expect(model.state == .needsAuthorization)
    }

    @Test("defaults to a valid grid density")
    func defaultDensity() {
        let model = TimelineViewModel(provider: MockAssetProvider())
        #expect(TimelineViewModel.columnOptions.contains(model.columnCount))
    }
}
