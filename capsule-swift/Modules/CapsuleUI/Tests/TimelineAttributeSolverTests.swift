import CoreGraphics
import Foundation
import Testing

@testable import CapsuleUI

// MARK: - Counting double

/// A ``TimelineGeometrySource`` that forwards to a real ``TimelineLayout`` and
/// counts what was asked of it.
///
/// The output of the solver cannot prove the property that matters. Two layouts
/// can return byte-identical attributes for a viewport while one of them walked
/// 250 000 items to get there — and that one makes the app unusable. Counting
/// the queries is the only way to assert the *cost*, so the cost is asserted
/// here rather than assumed.
private final class CountingGeometry: TimelineGeometrySource {
    let base: TimelineLayout

    private(set) var frameQueries = 0
    private(set) var indexRangeQueries = 0
    private(set) var sectionLookups = 0
    private(set) var sectionItemCountQueries = 0

    init(_ base: TimelineLayout) {
        self.base = base
    }

    var sectionCount: Int { base.sectionCount }
    var itemCount: Int { base.itemCount }
    var totalContentHeight: CGFloat { base.totalContentHeight }
    var headerHeight: CGFloat { base.headerHeight }
    var sectionSpacing: CGFloat { base.sectionSpacing }

    func itemCount(inSection section: Int) -> Int {
        sectionItemCountQueries += 1
        return base.itemCount(inSection: section)
    }

    func indexRange(intersecting rect: CGRect) -> Range<Int> {
        indexRangeQueries += 1
        return base.indexRange(intersecting: rect)
    }

    func frame(forGlobalIndex globalIndex: Int) -> CGRect? {
        frameQueries += 1
        return base.frame(forGlobalIndex: globalIndex)
    }

    func sectionIndex(forGlobalIndex globalIndex: Int) -> Int? {
        sectionLookups += 1
        return base.sectionIndex(forGlobalIndex: globalIndex)
    }

    func firstGlobalIndex(inSection section: Int) -> Int? {
        base.firstGlobalIndex(inSection: section)
    }

    func sectionIndex(atOffset offsetY: CGFloat) -> Int {
        base.sectionIndex(atOffset: offsetY)
    }

    func offset(forSection section: Int) -> CGFloat? {
        base.offset(forSection: section)
    }
}

// MARK: - Fixtures

private func metrics(columns: Int = 3) -> TimelineLayout.Metrics {
    TimelineLayout.Metrics(
        columns: columns,
        itemSide: 100,
        itemSpacing: 2,
        headerHeight: 40,
        sectionSpacing: 10,
        horizontalInset: 0
    )
}

private func layout(_ counts: [Int], columns: Int = 3) -> TimelineLayout {
    TimelineLayout(
        sections: counts.enumerated().map { .init(key: "day-\($0.offset)", count: $0.element) },
        metrics: metrics(columns: columns)
    )
}

/// Ten years of photos: 3 650 days averaging ~68 assets each.
private func hugeLayout() -> TimelineLayout {
    let sections = (0 ..< 3650).map { day in
        TimelineLayout.Section(key: "day-\(day)", count: 40 + (day * 7) % 90)
    }
    return TimelineLayout(sections: sections, metrics: .fitting(width: 1200, columns: 8))
}

// MARK: - Item attributes

@Suite("TimelineAttributeSolver items")
struct TimelineAttributeSolverItemTests {
    @Test("the attributes are exactly the indices the visible range names")
    func attributesMatchVisibleRange() {
        let subject = layout([7, 12, 5, 9])
        let rect = CGRect(x: 0, y: 120, width: 304, height: 400)

        let expected = subject.indexRange(intersecting: rect)
        let placements = TimelineAttributeSolver.itemPlacements(in: rect, geometry: subject)

        #expect(placements.map(\.globalIndex) == Array(expected))
        for placement in placements {
            #expect(placement.frame == subject.frame(forGlobalIndex: placement.globalIndex))
        }
    }

    @Test("every attribute's index path round-trips to its own global index")
    func indexPathsRoundTrip() {
        let subject = layout([7, 12, 5, 9])
        let rect = CGRect(x: 0, y: 0, width: 304, height: 1200)

        for placement in TimelineAttributeSolver.itemPlacements(in: rect, geometry: subject) {
            let start = subject.firstGlobalIndex(inSection: placement.section)
            #expect(start != nil)
            #expect((start ?? 0) + placement.item == placement.globalIndex)
            #expect(subject.sectionIndex(forGlobalIndex: placement.globalIndex) == placement.section)
            // The section-boundary walk must never run past a section's end.
            #expect(placement.item < subject.itemCount(inSection: placement.section))
        }
    }

    @Test("addressing one item by index path agrees with addressing it globally")
    func singleItemLookup() {
        let subject = layout([7, 12, 5])
        let placement = TimelineAttributeSolver.itemPlacement(section: 1, item: 4, geometry: subject)
        #expect(placement?.globalIndex == 11)
        #expect(placement?.frame == subject.frame(forGlobalIndex: 11))
        // Out of range on either axis is nil, not a trap: a layout and a data
        // source can disagree for one frame after a change.
        #expect(TimelineAttributeSolver.itemPlacement(section: 1, item: 12, geometry: subject) == nil)
        #expect(TimelineAttributeSolver.itemPlacement(section: 9, item: 0, geometry: subject) == nil)
    }

    @Test("a rect below the content answers with nothing rather than the last page")
    func emptyRects() {
        let subject = layout([4])
        let zeroHeight = CGRect(x: 0, y: 0, width: 304, height: 0)
        #expect(TimelineAttributeSolver.itemPlacements(in: zeroHeight, geometry: subject).isEmpty)
        #expect(TimelineAttributeSolver.itemPlacements(in: .zero, geometry: layout([])).isEmpty)
    }
}

// MARK: - The cost claim

@Suite("TimelineAttributeSolver cost")
struct TimelineAttributeSolverCostTests {
    @Test("answering a viewport costs one frame query per visible item and no more")
    func costIsPerVisibleItem() {
        let subject = hugeLayout()
        let counting = CountingGeometry(subject)
        let rect = CGRect(x: 0, y: subject.totalContentHeight / 2, width: 1200, height: 900)

        let expected = subject.indexRange(intersecting: rect)
        let placements = TimelineAttributeSolver.itemPlacements(in: rect, geometry: counting)

        #expect(placements.count == expected.count)
        // The assertion this suite exists for: the frame maths ran once per
        // *visible* item, never once per item in the library.
        #expect(counting.frameQueries == expected.count)
        #expect(counting.frameQueries < subject.itemCount / 100)
        // And the search for where the viewport starts ran once, not per item.
        #expect(counting.indexRangeQueries == 1)
        #expect(counting.sectionLookups == 1)
    }

    @Test("the section walk touches only the sections the viewport spans")
    func costIsPerVisibleSection() {
        let subject = hugeLayout()
        let counting = CountingGeometry(subject)
        let rect = CGRect(x: 0, y: subject.totalContentHeight / 3, width: 1200, height: 900)

        _ = TimelineAttributeSolver.itemPlacements(in: rect, geometry: counting)
        let spanned = TimelineAttributeSolver.sectionRange(intersecting: rect, geometry: subject)

        // One count per section entered, plus the one the walk starts in.
        #expect(counting.sectionItemCountQueries <= spanned.count + 1)
        #expect(counting.sectionItemCountQueries < subject.sectionCount)
    }

    @Test("scrolling the whole library never grows the per-frame cost")
    func costIsFlatAcrossTheLibrary() {
        let subject = hugeLayout()
        for step in 0 ..< 40 {
            let counting = CountingGeometry(subject)
            let scrollY = subject.totalContentHeight * CGFloat(step) / 40
            let rect = CGRect(x: 0, y: scrollY, width: 1200, height: 900)
            _ = TimelineAttributeSolver.itemPlacements(in: rect, geometry: counting)
            // Eight columns over a 900pt viewport is on the order of tens of
            // items — the same at the top of the decade and at the bottom.
            #expect(counting.frameQueries < 200, "frame at \(scrollY) cost \(counting.frameQueries) queries")
        }
    }
}

// MARK: - Headers

@Suite("TimelineAttributeSolver headers")
struct TimelineAttributeSolverHeaderTests {
    @Test("an unpinned header sits at its own offset")
    func unpinnedHeader() {
        let subject = layout([4, 4])
        let placement = TimelineAttributeSolver.headerPlacement(
            section: 1, width: 304, contentTop: 9999, pinsHeaders: false, geometry: subject
        )
        #expect(placement?.frame.minY == subject.offset(forSection: 1))
        #expect(placement?.isPinned == false)
        #expect(placement?.frame.height == 40)
    }

    @Test("a pinned header follows the viewport down through its own section")
    func pinnedHeaderFollowsViewport() {
        let subject = layout([12])
        let top = subject.offset(forSection: 0) ?? 0
        let atRest = TimelineAttributeSolver.headerPlacement(
            section: 0, width: 304, contentTop: 0, pinsHeaders: true, geometry: subject
        )
        #expect(atRest?.frame.minY == top)
        #expect(atRest?.isPinned == false)

        let scrolled = TimelineAttributeSolver.headerPlacement(
            section: 0, width: 304, contentTop: 120, pinsHeaders: true, geometry: subject
        )
        #expect(scrolled?.frame.minY == 120)
        #expect(scrolled?.isPinned == true)
    }

    @Test("a pinned header is pushed out by the end of its own section, not by the gap")
    func pinnedHeaderStopsAtSectionEnd() {
        let subject = layout([4, 4])
        // Far past section 0, which is a 40pt header over two 100pt rows.
        let placement = TimelineAttributeSolver.headerPlacement(
            section: 0, width: 304, contentTop: 5000, pinsHeaders: true, geometry: subject
        )
        // Section 0's content bottom is 242; a 40pt header comes to rest at 202
        // and never hangs into the 10pt gap below it.
        #expect(subject.contentBottom(ofSection: 0) == 242)
        #expect(placement?.frame.minY == 202)
        #expect(placement?.isPinned == true)
    }

    @Test("headers are hidden entirely when the metrics reserve no height")
    func hiddenHeaders() {
        let subject = TimelineLayout(
            sections: [.init(key: "day-0", count: 4)],
            metrics: TimelineLayout.Metrics(columns: 3, itemSide: 100, headerHeight: 0)
        )
        let rect = CGRect(x: 0, y: 0, width: 304, height: 400)
        #expect(TimelineAttributeSolver.headerPlacements(
            in: rect, width: 304, contentTop: 0, pinsHeaders: true, geometry: subject
        ).isEmpty)
    }

    @Test("only the sections the viewport touches get a header")
    func headerRangeIsBounded() {
        let subject = hugeLayout()
        let rect = CGRect(x: 0, y: subject.totalContentHeight / 2, width: 1200, height: 900)
        let placements = TimelineAttributeSolver.headerPlacements(
            in: rect, width: 1200, contentTop: rect.minY, pinsHeaders: true, geometry: subject
        )
        #expect(!placements.isEmpty)
        #expect(placements.count < 20, "a viewport should not lay out \(placements.count) headers")
        #expect(placements.map(\.section) == Array(
            TimelineAttributeSolver.sectionRange(intersecting: rect, geometry: subject)
        ))
    }
}

// MARK: - Section boundary regression

@Suite("TimelineLayout section boundaries")
struct TimelineLayoutBoundaryTests {
    /// A viewport whose top lands in the gap *between* two days.
    ///
    /// The row-aligned skip inside `indexRange(intersecting:)` used to be clamped
    /// to `rows × columns`, which for a ragged last row overshoots the section's
    /// own item count and silently skips that many items of the *next* section.
    /// On screen that is the first tiles under a day boundary rendering blank
    /// while everything around them is fine — the hardest kind of grid bug to
    /// see in a screenshot and the easiest to ship.
    @Test("a viewport starting in the inter-section gap still shows the next day's first row")
    func gapStartIncludesNextSectionsFirstRow() {
        // Section 0 holds 4 items over 2 rows of 3 — a ragged last row.
        let subject = layout([4, 6])
        let sectionOneTop = subject.offset(forSection: 1) ?? 0
        // Just above the next section's header: inside the gap.
        let rect = CGRect(x: 0, y: sectionOneTop - 6, width: 304, height: 400)
        let range = subject.indexRange(intersecting: rect)

        #expect(range.contains(4), "the next day's first item is visible but outside \(range)")
        #expect(range.contains(5))
        for index in 0 ..< subject.itemCount {
            guard let frame = subject.frame(forGlobalIndex: index) else { continue }
            if frame.intersects(rect) {
                #expect(range.contains(index), "item \(index) is visible but outside \(range)")
            }
        }
    }

    @Test("every viewport across a ragged library covers every intersecting item")
    func raggedLibraryIsFullyCovered() {
        let subject = layout([1, 4, 2, 7, 3, 5], columns: 3)
        for step in 0 ..< 60 {
            let rect = CGRect(
                x: 0,
                y: subject.totalContentHeight * CGFloat(step) / 60,
                width: 304,
                height: 260
            )
            let range = subject.indexRange(intersecting: rect)
            for index in 0 ..< subject.itemCount {
                guard let frame = subject.frame(forGlobalIndex: index) else { continue }
                if frame.intersects(rect) {
                    #expect(range.contains(index), "item \(index) missing from \(range) at \(rect.minY)")
                }
            }
        }
    }
}

// MARK: - Grid geometry

@Suite("TimelineGridGeometry")
struct TimelineGridGeometryTests {
    private func aggregate(_ counts: [Int]) -> [TimelineGridSection] {
        counts.enumerated().map { .init(key: "day-\($0.offset)", title: "Day \($0.offset)", count: $0.element) }
    }

    @Test("the item count is the aggregate's sum, known without any assets")
    func itemCountFromAggregate() {
        let geometry = TimelineGridGeometry(sections: aggregate([3, 4, 5]), columns: 3)
        #expect(geometry.itemCount == 12)
    }

    @Test("resolving against a width fills it exactly")
    func resolvesToWidth() {
        let geometry = TimelineGridGeometry(sections: aggregate([9]), columns: 3, itemSpacing: 2)
        let resolved = geometry.layout(forWidth: 304)
        #expect(abs(resolved.metrics.itemSide * 3 + 2 * 2 - 304) < 0.001)
        #expect(resolved.itemCount == 9)
    }

    @Test("hiding headers removes their height from the content, not just their view")
    func hiddenHeadersShrinkContent() {
        let shown = TimelineGridGeometry(sections: aggregate([3]), columns: 3, showsHeaders: true)
        let hidden = TimelineGridGeometry(sections: aggregate([3]), columns: 3, showsHeaders: false)
        let difference = shown.layout(forWidth: 300).totalContentHeight
            - hidden.layout(forWidth: 300).totalContentHeight
        #expect(abs(difference - shown.headerHeight) < 0.001)
    }

    @Test("a screenful is counted in whole rows of the current density")
    func viewportItemCount() {
        let geometry = TimelineGridGeometry(sections: aggregate([100]), columns: 4, itemSpacing: 0)
        // A 400pt viewport over 100pt tiles is 4 rows of 4.
        #expect(geometry.viewportItemCount(height: 400, itemSide: 100) == 16)
        // Never zero, however small the viewport: the store's margin is
        // denominated in screenfuls and a screenful of nothing reads nothing.
        #expect(geometry.viewportItemCount(height: 0, itemSide: 100) == 4)
    }

    @Test("a section title is addressable by index and nil outside the aggregate")
    func sectionTitles() {
        let geometry = TimelineGridGeometry(sections: aggregate([1, 1]), columns: 3)
        #expect(geometry.title(ofSection: 1) == "Day 1")
        #expect(geometry.title(ofSection: 2) == nil)
    }
}
