import CoreGraphics
import Foundation
import Testing

@testable import CapsuleUI

@Suite("TimelineLayout geometry")
struct TimelineLayoutTests {
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

    @Test("an empty layout has no items and no height")
    func emptyLayout() {
        let subject = layout([])
        #expect(subject.isEmpty)
        #expect(subject.itemCount == 0)
        #expect(subject.totalContentHeight == 0)
        #expect(subject.indexRange(intersecting: CGRect(x: 0, y: 0, width: 300, height: 800)).isEmpty)
    }

    @Test("total height is exact and independent of how many assets are loaded")
    func exactContentHeight() {
        // One section of 3 items (1 row) and one of 4 (2 rows), 3 columns.
        let subject = layout([3, 4])
        // Section 0: header 40 + row 100. Section 1: header 40 + 2 rows (100+2+100).
        // Plus one inter-section gap of 10; the trailing gap is slack, not content.
        let expected: CGFloat = (40 + 100) + 10 + (40 + 202)
        #expect(subject.totalContentHeight == expected)
    }

    @Test("item frames tile left-to-right then top-to-bottom within a section")
    func itemFrames() {
        let subject = layout([4])
        #expect(subject.frame(forGlobalIndex: 0) == CGRect(x: 0, y: 40, width: 100, height: 100))
        #expect(subject.frame(forGlobalIndex: 1) == CGRect(x: 102, y: 40, width: 100, height: 100))
        #expect(subject.frame(forGlobalIndex: 2) == CGRect(x: 204, y: 40, width: 100, height: 100))
        // Fourth item wraps to the next row.
        #expect(subject.frame(forGlobalIndex: 3) == CGRect(x: 0, y: 142, width: 100, height: 100))
        #expect(subject.frame(forGlobalIndex: 4) == nil)
    }

    @Test("a global index resolves to the section that contains it")
    func sectionLookup() {
        let subject = layout([3, 1, 5])
        #expect(subject.sectionIndex(forGlobalIndex: 0) == 0)
        #expect(subject.sectionIndex(forGlobalIndex: 2) == 0)
        #expect(subject.sectionIndex(forGlobalIndex: 3) == 1)
        #expect(subject.sectionIndex(forGlobalIndex: 4) == 2)
        #expect(subject.sectionIndex(forGlobalIndex: 8) == 2)
        #expect(subject.sectionIndex(forGlobalIndex: 9) == nil)
        #expect(subject.sectionIndex(forGlobalIndex: -1) == nil)
    }

    @Test("the visible range covers every item intersecting the viewport")
    func visibleRangeCoversViewport() {
        let subject = layout([30], columns: 3)
        let viewport = CGRect(x: 0, y: 0, width: 304, height: 300)
        let range = subject.indexRange(intersecting: viewport)

        // Every item whose frame intersects the viewport must be inside the range —
        // a missed item is a blank tile on screen, which is the bug this guards.
        for index in 0 ..< subject.itemCount {
            guard let frame = subject.frame(forGlobalIndex: index) else { continue }
            if frame.intersects(viewport) {
                #expect(range.contains(index), "item \(index) is visible but outside \(range)")
            }
        }
        // And it must not be the whole library.
        #expect(range.count < subject.itemCount)
    }

    @Test("the visible range is bounded by the item count at the very bottom")
    func visibleRangeClampsAtTheEnd() {
        let subject = layout([7])
        let bottom = CGRect(x: 0, y: subject.totalContentHeight - 50, width: 304, height: 400)
        let range = subject.indexRange(intersecting: bottom)
        #expect(range.upperBound <= subject.itemCount)
        #expect(range.lowerBound <= range.upperBound)
    }

    @Test("scrubbing to an offset reports the section key at that point")
    func scrubberKeys() {
        let subject = layout([3, 4, 2])
        #expect(subject.sectionKey(atOffset: 0) == "day-0")
        #expect(subject.sectionKey(atOffset: subject.totalContentHeight) == "day-2")
        let secondSectionTop = subject.offset(forSection: 1) ?? 0
        #expect(subject.sectionKey(atOffset: secondSectionTop + 1) == "day-1")
    }

    @Test("the nearest item to a point is inside the section at that offset")
    func nearestItem() {
        let subject = layout([3, 4, 2])
        let secondSectionTop = subject.offset(forSection: 1) ?? 0
        let point = CGPoint(x: 150, y: secondSectionTop + 60)
        let index = subject.globalIndex(nearest: point)
        #expect(index != nil)
        #expect(subject.sectionIndex(forGlobalIndex: index ?? -1) == 1)
    }

    @Test("fitting metrics divide the width so the last column lands flush")
    func fittingMetrics() {
        let fitted = TimelineLayout.Metrics.fitting(width: 320, columns: 3, itemSpacing: 4)
        let totalWidth = fitted.itemSide * 3 + fitted.itemSpacing * 2
        #expect(abs(totalWidth - 320) < 0.001)
    }
}

@Suite("TimelineLayout at library scale")
struct TimelineLayoutScaleTests {
    /// Ten years of photos: one section per day, averaging ~68 assets a day, so
    /// a quarter of a million assets across 3 650 sections.
    private func hugeLayout() -> TimelineLayout {
        let sections = (0 ..< 3650).map { day in
            TimelineLayout.Section(key: "day-\(day)", count: 40 + (day * 7) % 90)
        }
        return TimelineLayout(
            sections: sections,
            metrics: .fitting(width: 1200, columns: 8)
        )
    }

    @Test("builds a decade of sections quickly")
    func buildsQuickly() {
        let clock = ContinuousClock()
        let elapsed = clock.measure { _ = hugeLayout() }
        // Generous: this runs on CI hardware too. The point is that it is
        // milliseconds, not the seconds a per-item layout pass would cost.
        #expect(elapsed < .milliseconds(250))
    }

    @Test("a viewport query stays cheap on a quarter-million assets")
    func viewportQueryIsCheap() {
        let subject = hugeLayout()
        #expect(subject.itemCount > 200000)

        let clock = ContinuousClock()
        let elapsed = clock.measure {
            // Sample the whole scroll extent, as a fast flick would.
            for step in 0 ..< 1000 {
                let scrollY = subject.totalContentHeight * CGFloat(step) / 1000
                _ = subject.indexRange(intersecting: CGRect(x: 0, y: scrollY, width: 1200, height: 900))
            }
        }
        #expect(elapsed < .milliseconds(250))
    }

    @Test("a visible window is a screenful, never the whole library")
    func visibleWindowStaysSmall() {
        let subject = hugeLayout()
        let range = subject.indexRange(
            intersecting: CGRect(x: 0, y: subject.totalContentHeight / 2, width: 1200, height: 900)
        )
        // Eight columns over a 900pt viewport is on the order of tens of items.
        #expect(range.count < 200, "a viewport should not materialize \(range.count) items")
    }

    @Test("seeking to any section is exact")
    func seekIsExact() {
        let subject = hugeLayout()
        for section in stride(from: 0, to: 3650, by: 337) {
            guard let offset = subject.offset(forSection: section) else {
                Issue.record("no offset for section \(section)")
                continue
            }
            #expect(subject.sectionIndex(atOffset: offset) == section)
            #expect(subject.sectionKey(atOffset: offset) == "day-\(section)")
        }
    }
}
