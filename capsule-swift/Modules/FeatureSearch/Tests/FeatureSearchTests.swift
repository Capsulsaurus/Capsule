import Foundation
import Testing

import AssetKit
import CapsuleTestSupport
import FeatureSearch

@Suite("SearchFilter")
struct SearchFilterTests {
    private var utcCalendar: Calendar {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = .gmt
        return calendar
    }

    @Test("an empty filter is inactive and matches everything")
    func emptyFilterMatchesAll() {
        let filter = SearchFilter()
        #expect(filter.isActive == false)
        #expect(filter.matches(Fixtures.asset(mediaType: .video)))
    }

    @Test("a media-type facet excludes other types")
    func mediaTypeFacet() {
        let filter = SearchFilter(mediaType: .video)
        #expect(filter.isActive)
        #expect(filter.matches(Fixtures.asset(mediaType: .video)))
        #expect(filter.matches(Fixtures.asset(mediaType: .photo)) == false)
    }

    @Test("a date facet excludes assets outside the window")
    func dateFacet() {
        // 2024-07-03 UTC.
        let reference = Date(timeIntervalSince1970: 1_720_000_000)
        let filter = SearchFilter(dateRange: .thisYear)
        let thisYear = Fixtures.asset(captureDate: reference)
        let lastYear = Fixtures.asset(captureDate: reference.addingTimeInterval(-400 * 86_400))

        #expect(filter.matches(thisYear, referenceDate: reference, calendar: utcCalendar))
        #expect(filter.matches(lastYear, referenceDate: reference, calendar: utcCalendar) == false)
    }
}

@Suite("SearchViewModel")
@MainActor
struct SearchViewModelTests {
    @Test("filters the loaded timeline by media type")
    func filtersByMediaType() async {
        let assets = [
            Fixtures.asset(mediaType: .photo),
            Fixtures.asset(mediaType: .video),
            Fixtures.asset(mediaType: .photo),
        ]
        let model = SearchViewModel(provider: MockAssetProvider(assets: assets))
        await model.load()
        #expect(model.results.count == 3)

        model.filter = SearchFilter(mediaType: .video)

        #expect(model.results.count == 1)
        #expect(model.results.first?.mediaType == .video)
    }

    @Test("clearing the filter restores every result")
    func clearingFilterRestoresAll() async {
        let model = SearchViewModel(provider: MockAssetProvider(assets: Fixtures.assets(count: 4)))
        await model.load()
        model.filter = SearchFilter(mediaType: .video)
        #expect(model.results.isEmpty)

        model.filter = SearchFilter()

        #expect(model.results.count == 4)
    }
}
