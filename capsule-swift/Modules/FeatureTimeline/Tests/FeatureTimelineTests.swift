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
        let dayOne = Date(timeIntervalSince1970: 1_720_000_000)
        let dayTwo = Date(timeIntervalSince1970: 1_720_100_000)
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

    @Test("the current and prior days are titled Today and Yesterday")
    func relativeDayTitles() {
        let today = Date(timeIntervalSince1970: 1_720_000_000)
        let yesterday = today.addingTimeInterval(-86_400)
        let sections = TimelineSectioning.sections(
            from: [Fixtures.asset(captureDate: today), Fixtures.asset(captureDate: yesterday)],
            calendar: utcCalendar,
            referenceDate: today
        )
        #expect(sections.first?.title == "Today")
        #expect(sections.last?.title == "Yesterday")
    }

    @Test("same-day assets collapse into a single section")
    func sameDayCollapses() {
        let base = Date(timeIntervalSince1970: 1_720_000_000)
        let assets = (0 ..< 4).map { Fixtures.asset(captureDate: base.addingTimeInterval(Double($0) * 600)) }
        let sections = TimelineSectioning.sections(from: assets, calendar: utcCalendar, referenceDate: base)
        #expect(sections.count == 1)
        #expect(sections[0].assets.count == 4)
    }
}

@Suite("TimelineViewModel")
@MainActor
struct TimelineViewModelTests {
    @Test("loads and sections an authorized library")
    func loadsAuthorizedLibrary() async {
        let provider = MockAssetProvider(assets: Fixtures.assets(count: 6), status: .authorized)
        let model = TimelineViewModel(provider: provider)
        await model.load()
        #expect(model.state == .ready)
        // Six assets one day apart → six day sections.
        #expect(model.sections.count == 6)
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
