import CoreGraphics
import Foundation

/// The geometry of a sectioned photo grid, computed from section *counts*
/// alone.
///
/// This is the piece that makes a library of hundreds of thousands of assets
/// scrollable. Its input is not assets — it is one `(dayKey, count)` row per
/// day, which for ten years of photos is a few thousand rows and a few
/// kilobytes. From that it precomputes prefix sums of item counts and vertical
/// offsets, so three things become cheap that are otherwise impossible:
///
/// - **Total content height is exact before a single asset is loaded.** A grid
///   that grows its content size as pages arrive has a scrollbar that lies and a
///   scroll position that jumps; this one does not.
/// - **`indexRange(intersecting:)` is a binary search**, so the view can ask
///   "which items are on screen?" every frame without touching the data source.
/// - **Jumping to a date is O(log n)** — the fast-scroll scrubber can seek to
///   October 2019 in a 250 000-asset library instantly, because the offset of
///   that day is already known.
///
/// It is a pure value type with no view, no store, and no platform dependency,
/// which is what makes the maths directly testable. `AssetWindowStore` supplies
/// the assets for whatever range this says is visible.
public struct TimelineLayout: Sendable, Equatable {
    // MARK: Inputs

    /// One section's identity and size — the aggregate the local index can
    /// answer with a `GROUP BY` rather than a full scan.
    public struct Section: Sendable, Equatable {
        /// The section's stable key, e.g. an ISO day (`"2026-08-22"`).
        public let key: String
        /// How many assets the section holds. Sections are never empty.
        public let count: Int

        public init(key: String, count: Int) {
            self.key = key
            self.count = count
        }
    }

    /// The visual metrics the layout is computed against.
    ///
    /// Separated from the sections so a resize or a density change recomputes
    /// geometry without re-querying the index.
    public struct Metrics: Sendable, Equatable {
        /// Items per row. Always at least 1.
        public let columns: Int
        /// The side of one square tile, in points.
        public let itemSide: CGFloat
        /// Gap between tiles, horizontally and vertically.
        public let itemSpacing: CGFloat
        /// Height of a section header, or 0 when headers are hidden.
        public let headerHeight: CGFloat
        /// Vertical gap above each section's header.
        public let sectionSpacing: CGFloat
        /// Horizontal inset applied to both edges of the content.
        public let horizontalInset: CGFloat

        public init(
            columns: Int,
            itemSide: CGFloat,
            itemSpacing: CGFloat = 2,
            headerHeight: CGFloat = 44,
            sectionSpacing: CGFloat = 16,
            horizontalInset: CGFloat = 0
        ) {
            self.columns = max(1, columns)
            self.itemSide = itemSide
            self.itemSpacing = itemSpacing
            self.headerHeight = headerHeight
            self.sectionSpacing = sectionSpacing
            self.horizontalInset = horizontalInset
        }

        /// Derive metrics that fill `width` with `columns` square tiles.
        ///
        /// The tile side falls out of the available width rather than being
        /// chosen independently, so the trailing column always lands flush with
        /// the content edge — a grid whose last column is a few points short is
        /// the most visible way a photo grid looks wrong.
        public static func fitting(
            width: CGFloat,
            columns: Int,
            itemSpacing: CGFloat = 2,
            headerHeight: CGFloat = 44,
            sectionSpacing: CGFloat = 16,
            horizontalInset: CGFloat = 0
        ) -> Metrics {
            let columns = max(1, columns)
            let available = max(0, width - horizontalInset * 2 - itemSpacing * CGFloat(columns - 1))
            return Metrics(
                columns: columns,
                itemSide: available / CGFloat(columns),
                itemSpacing: itemSpacing,
                headerHeight: headerHeight,
                sectionSpacing: sectionSpacing,
                horizontalInset: horizontalInset
            )
        }
    }

    public let sections: [Section]
    public let metrics: Metrics

    // MARK: Precomputed prefix sums

    /// `itemPrefix[i]` is the number of items before section `i`;
    /// `itemPrefix[count]` is the total. One extra element so the last section
    /// needs no special case.
    private let itemPrefix: [Int]
    /// `offsetPrefix[i]` is the y offset of section `i`'s header.
    private let offsetPrefix: [CGFloat]

    /// The total height of the content, exactly.
    public let totalContentHeight: CGFloat

    // MARK: Construction

    public init(sections: [Section], metrics: Metrics) {
        self.sections = sections
        self.metrics = metrics

        var items = [Int](repeating: 0, count: sections.count + 1)
        var offsets = [CGFloat](repeating: 0, count: sections.count + 1)
        var runningItems = 0
        var offsetY: CGFloat = 0
        for (index, section) in sections.enumerated() {
            items[index] = runningItems
            offsets[index] = offsetY
            runningItems += section.count
            offsetY += metrics.headerHeight
            offsetY += Self.bodyHeight(itemCount: section.count, metrics: metrics)
            offsetY += metrics.sectionSpacing
        }
        items[sections.count] = runningItems
        offsets[sections.count] = offsetY
        itemPrefix = items
        offsetPrefix = offsets
        // Trailing section spacing is layout slack, not content.
        totalContentHeight = max(0, offsetY - (sections.isEmpty ? 0 : metrics.sectionSpacing))
    }

    /// The height of a section's tiles, excluding its header.
    private static func bodyHeight(itemCount: Int, metrics: Metrics) -> CGFloat {
        guard itemCount > 0 else { return 0 }
        let rows = (itemCount + metrics.columns - 1) / metrics.columns
        return CGFloat(rows) * metrics.itemSide + CGFloat(rows - 1) * metrics.itemSpacing
    }

    // MARK: Queries

    /// Total number of items across every section.
    public var itemCount: Int { itemPrefix[sections.count] }

    /// Whether the layout has nothing to show.
    public var isEmpty: Bool { itemCount == 0 }

    /// The index of the section containing `globalIndex`, or `nil` if out of range.
    ///
    /// Binary search over the item prefix sums — O(log n) in the number of
    /// *sections*, independent of the number of assets.
    public func sectionIndex(forGlobalIndex globalIndex: Int) -> Int? {
        guard globalIndex >= 0, globalIndex < itemCount else { return nil }
        var low = 0
        var high = sections.count - 1
        while low < high {
            let mid = (low + high + 1) / 2
            if itemPrefix[mid] <= globalIndex { low = mid } else { high = mid - 1 }
        }
        return low
    }

    /// The global index of the first item in `section`.
    public func firstGlobalIndex(inSection section: Int) -> Int? {
        guard section >= 0, section < sections.count else { return nil }
        return itemPrefix[section]
    }

    /// The frame of the item at `globalIndex`.
    public func frame(forGlobalIndex globalIndex: Int) -> CGRect? {
        guard let section = sectionIndex(forGlobalIndex: globalIndex) else { return nil }
        let indexInSection = globalIndex - itemPrefix[section]
        let row = indexInSection / metrics.columns
        let column = indexInSection % metrics.columns
        let originX = metrics.horizontalInset
            + CGFloat(column) * (metrics.itemSide + metrics.itemSpacing)
        let originY = offsetPrefix[section]
            + metrics.headerHeight
            + CGFloat(row) * (metrics.itemSide + metrics.itemSpacing)
        return CGRect(x: originX, y: originY, width: metrics.itemSide, height: metrics.itemSide)
    }

    /// The frame of `section`'s header.
    public func headerFrame(forSection section: Int, width: CGFloat) -> CGRect? {
        guard section >= 0, section < sections.count else { return nil }
        return CGRect(
            x: 0,
            y: offsetPrefix[section],
            width: width,
            height: metrics.headerHeight
        )
    }

    /// The global item indices intersecting `rect`.
    ///
    /// The range this returns is what the window store fetches and what the
    /// collection view renders; everything outside it costs nothing.
    public func indexRange(intersecting rect: CGRect) -> Range<Int> {
        guard !sections.isEmpty, rect.height > 0 else { return 0 ..< 0 }
        let first = sectionIndex(atOffset: rect.minY)
        let last = sectionIndex(atOffset: rect.maxY)

        // Within the first intersecting section, skip whole rows above the rect.
        let lowerBound = itemPrefix[first] + rowAlignedItemOffset(
            inSection: first, offsetY: rect.minY, rounding: .downward
        )
        let upperBound = itemPrefix[last] + rowAlignedItemOffset(
            inSection: last, offsetY: rect.maxY, rounding: .upward
        )
        let clampedLower = max(0, min(lowerBound, itemCount))
        let clampedUpper = max(clampedLower, min(upperBound, itemCount))
        return clampedLower ..< clampedUpper
    }

    /// The section whose vertical span contains `y`, clamped to the ends.
    public func sectionIndex(atOffset offsetY: CGFloat) -> Int {
        guard !sections.isEmpty else { return 0 }
        if offsetY <= 0 { return 0 }
        if offsetY >= totalContentHeight { return sections.count - 1 }
        var low = 0
        var high = sections.count - 1
        while low < high {
            let mid = (low + high + 1) / 2
            if offsetPrefix[mid] <= offsetY { low = mid } else { high = mid - 1 }
        }
        return low
    }

    /// The section key at a vertical offset — what the fast-scroll scrubber
    /// shows while the user drags.
    public func sectionKey(atOffset offsetY: CGFloat) -> String? {
        guard !sections.isEmpty else { return nil }
        return sections[sectionIndex(atOffset: offsetY)].key
    }

    /// The y offset to scroll to in order to bring `section`'s header to the top.
    public func offset(forSection section: Int) -> CGFloat? {
        guard section >= 0, section < sections.count else { return nil }
        return offsetPrefix[section]
    }

    /// The item nearest the given point — used to keep the user's place when
    /// the zoom level changes.
    ///
    /// Zooming between Days and All rebuilds the layout entirely; scrolling to
    /// this item's index in the new layout is what makes the transition feel
    /// like staying in one place rather than jumping to the top.
    public func globalIndex(nearest point: CGPoint) -> Int? {
        guard !isEmpty else { return nil }
        let section = sectionIndex(atOffset: point.y)
        let offset = rowAlignedItemOffset(inSection: section, offsetY: point.y, rounding: .downward)
        let column = columnIndex(atX: point.x)
        let candidate = itemPrefix[section] + offset + column
        return min(max(0, candidate), itemCount - 1)
    }

    // MARK: Helpers

    /// Whether a partially-covered row counts as inside the range.
    private enum Rounding { case upward, downward }

    /// How many items precede `y` within `section`, aligned to whole rows.
    private func rowAlignedItemOffset(inSection section: Int, offsetY: CGFloat, rounding: Rounding) -> Int {
        let bodyTop = offsetPrefix[section] + metrics.headerHeight
        let stride = metrics.itemSide + metrics.itemSpacing
        guard stride > 0 else { return 0 }
        let relative = offsetY - bodyTop
        if relative <= 0 { return 0 }
        let row = relative / stride
        let rowIndex = rounding == .downward ? Int(row.rounded(.down)) : Int(row.rounded(.up))
        let clamped = max(0, min(rowIndex, rowCount(inSection: section)))
        // Clamped to the section's *item* count, not to `rows × columns`. A
        // ragged last row makes those differ, and the difference is not
        // cosmetic: a `y` in the gap below a section resolves to its full row
        // count, and an unclamped `rows × columns` would then run past the
        // section's end and silently skip that many items of the *next*
        // section — the first rows under a section boundary rendering blank.
        return min(clamped * metrics.columns, sections[section].count)
    }

    private func rowCount(inSection section: Int) -> Int {
        let count = sections[section].count
        return (count + metrics.columns - 1) / metrics.columns
    }

    private func columnIndex(atX positionX: CGFloat) -> Int {
        let stride = metrics.itemSide + metrics.itemSpacing
        guard stride > 0 else { return 0 }
        let relative = max(0, positionX - metrics.horizontalInset)
        return min(metrics.columns - 1, Int((relative / stride).rounded(.down)))
    }
}
