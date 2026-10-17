import CoreGraphics
import Foundation

// MARK: - Placements

/// Where one item sits, in the terms a collection view speaks.
///
/// A platform-neutral stand-in for `UICollectionViewLayoutAttributes` /
/// `NSCollectionViewLayoutAttributes`: the two classes are spelled differently,
/// are reference types, and cannot be constructed off a Mac test run without
/// dragging a whole framework in. The maths produces these; the island's layout
/// subclasses copy them into the platform's own attribute objects and do nothing
/// else.
public struct TimelineItemPlacement: Sendable, Equatable {
    /// The collection-view section — the same index the timeline uses.
    public let section: Int
    /// The item's index inside its section.
    public let item: Int
    /// The item's index across the whole timeline.
    public let globalIndex: Int
    /// Where it sits in content coordinates.
    public let frame: CGRect

    public init(section: Int, item: Int, globalIndex: Int, frame: CGRect) {
        self.section = section
        self.item = item
        self.globalIndex = globalIndex
        self.frame = frame
    }
}

/// Where one section header sits, after pinning.
public struct TimelineHeaderPlacement: Sendable, Equatable {
    public let section: Int
    public let frame: CGRect
    /// Whether the header is currently floating above its own content rather
    /// than sitting at its natural offset. Drives the z-order: a pinned header
    /// overlaps tiles and has to win.
    public let isPinned: Bool

    public init(section: Int, frame: CGRect, isPinned: Bool) {
        self.section = section
        self.frame = frame
        self.isPinned = isPinned
    }
}

// MARK: - TimelineAttributeSolver

/// Turns a viewport rectangle into the attributes a collection view needs, and
/// nothing more.
///
/// **This is the whole reason the timeline scales.** The conventional
/// `UICollectionViewLayout` computes every item's frame in `prepare()` and
/// caches them in a dictionary; at 250 000 items that is a quarter of a million
/// `CGRect`s built before the first row appears, ~10 MB of attribute objects
/// held for the life of the screen, and a full recomputation on every rotation.
///
/// Here `prepare()` builds nothing per item. A query does exactly three things:
///
/// 1. one binary search (``TimelineGeometrySource/indexRange(intersecting:)``)
///    to name the visible indices,
/// 2. one more (``TimelineGeometrySource/sectionIndex(forGlobalIndex:)``) to
///    find which section the first of them lives in, and
/// 3. one `frame(forGlobalIndex:)` per **visible** item — a few dozen — walking
///    the section boundary forward rather than re-searching for each.
///
/// The cost of a frame is therefore a function of the viewport, not of the
/// library, and it is the same whether the user is at the top of the grid or ten
/// years down it.
public enum TimelineAttributeSolver {
    // MARK: Items

    /// Every item intersecting `rect`, in ascending global order.
    public static func itemPlacements(
        in rect: CGRect,
        geometry: some TimelineGeometrySource
    ) -> [TimelineItemPlacement] {
        let range = geometry.indexRange(intersecting: rect)
        guard !range.isEmpty, let first = geometry.sectionIndex(forGlobalIndex: range.lowerBound) else {
            return []
        }

        var placements = [TimelineItemPlacement]()
        placements.reserveCapacity(range.count)

        // The section boundary is carried forward across the loop instead of
        // being re-derived per item: the range is contiguous and ascending, so
        // a second binary search per item would be pure waste.
        var section = first
        var sectionStart = geometry.firstGlobalIndex(inSection: section) ?? 0
        var sectionEnd = sectionStart + geometry.itemCount(inSection: section)

        for globalIndex in range {
            while globalIndex >= sectionEnd, section + 1 < geometry.sectionCount {
                section += 1
                sectionStart = sectionEnd
                sectionEnd = sectionStart + geometry.itemCount(inSection: section)
            }
            guard let frame = geometry.frame(forGlobalIndex: globalIndex) else { continue }
            placements.append(TimelineItemPlacement(
                section: section,
                item: globalIndex - sectionStart,
                globalIndex: globalIndex,
                frame: frame
            ))
        }
        return placements
    }

    /// One item, addressed the way a collection view addresses it.
    public static func itemPlacement(
        section: Int,
        item: Int,
        geometry: some TimelineGeometrySource
    ) -> TimelineItemPlacement? {
        guard item >= 0, item < geometry.itemCount(inSection: section),
              let start = geometry.firstGlobalIndex(inSection: section)
        else {
            return nil
        }
        let globalIndex = start + item
        guard let frame = geometry.frame(forGlobalIndex: globalIndex) else { return nil }
        return TimelineItemPlacement(section: section, item: item, globalIndex: globalIndex, frame: frame)
    }

    // MARK: Headers

    /// The sections whose vertical span intersects `rect`.
    ///
    /// Derived from the two ends of the rectangle rather than by scanning, so a
    /// viewport in the middle of a decade costs two binary searches.
    public static func sectionRange(
        intersecting rect: CGRect,
        geometry: some TimelineGeometrySource
    ) -> Range<Int> {
        guard geometry.sectionCount > 0, rect.height > 0 else { return 0 ..< 0 }
        let first = geometry.sectionIndex(atOffset: rect.minY)
        let last = geometry.sectionIndex(atOffset: rect.maxY)
        return first ..< min(geometry.sectionCount, last + 1)
    }

    /// Every section header intersecting `rect`, already pinned.
    ///
    /// - Parameters:
    ///   - contentTop: the y offset of the top of the visible content, in
    ///     content coordinates. Ignored when `pinsHeaders` is false.
    ///   - pinsHeaders: whether a header sticks to the top of the viewport while
    ///     its own section scrolls beneath it.
    public static func headerPlacements(
        in rect: CGRect,
        width: CGFloat,
        contentTop: CGFloat,
        pinsHeaders: Bool,
        geometry: some TimelineGeometrySource
    ) -> [TimelineHeaderPlacement] {
        guard geometry.headerHeight > 0 else { return [] }
        return sectionRange(intersecting: rect, geometry: geometry).compactMap { section in
            headerPlacement(
                section: section,
                width: width,
                contentTop: contentTop,
                pinsHeaders: pinsHeaders,
                geometry: geometry
            )
        }
    }

    /// One section header, already pinned.
    public static func headerPlacement(
        section: Int,
        width: CGFloat,
        contentTop: CGFloat,
        pinsHeaders: Bool,
        geometry: some TimelineGeometrySource
    ) -> TimelineHeaderPlacement? {
        let height = geometry.headerHeight
        guard height > 0, section >= 0, section < geometry.sectionCount,
              let top = geometry.offset(forSection: section)
        else {
            return nil
        }
        guard pinsHeaders else {
            return TimelineHeaderPlacement(
                section: section,
                frame: CGRect(x: 0, y: top, width: width, height: height),
                isPinned: false
            )
        }
        // A pinned header follows the viewport down until its own section runs
        // out, then is pushed off by the next one instead of overhanging content
        // it does not describe.
        let limit = max(top, geometry.contentBottom(ofSection: section) - height)
        let originY = min(max(top, contentTop), limit)
        return TimelineHeaderPlacement(
            section: section,
            frame: CGRect(x: 0, y: originY, width: width, height: height),
            isPinned: originY > top
        )
    }
}
