import CoreGraphics
import Foundation

// MARK: - TimelineGridSection

/// One day of the timeline, as the *aggregate* the grid is built from.
///
/// Note what is absent: the assets. A day is a key, a title, and a count —
/// three values the local index answers with a `GROUP BY` — and that is enough
/// to size and lay out the entire grid. The assets arrive later, per screenful,
/// through ``AssetWindowStore``.
public struct TimelineGridSection: Sendable, Equatable, Identifiable {
    /// The stable day key, e.g. `"2026-08-22"`. Also the section's identity.
    public let key: String
    /// The header text, already formatted for the reader's locale by whoever
    /// owns the calendar. A date is not a catalog string.
    public let title: String
    /// How many assets the day holds. Never zero — an empty day is not a
    /// section.
    public let count: Int

    public var id: String { key }

    public init(key: String, title: String, count: Int) {
        self.key = key
        self.title = title
        self.count = count
    }
}

// MARK: - TimelineGridGeometry

/// Everything the grid needs to compute its geometry, and nothing that changes
/// per frame.
///
/// Held by the layout object rather than by the view, because the thing that
/// most often changes the geometry is a **width change**, and a width change
/// must not have to round-trip through SwiftUI to be answered. The layout
/// resolves this value against its own bounds in `prepare()` and rebuilds a
/// ``TimelineLayout`` only when the width or the aggregate actually moved.
///
/// Equatable so that resolution is skipped when SwiftUI re-renders with the
/// same inputs, which it does constantly.
public struct TimelineGridGeometry: Sendable, Equatable {
    /// The day aggregate, in display order.
    public var sections: [TimelineGridSection]
    /// Tiles per row.
    public var columns: Int
    /// The gap between tiles, in points.
    public var itemSpacing: CGFloat
    /// The height a section header reserves, when headers are shown.
    public var headerHeight: CGFloat
    /// The vertical gap between one day's last row and the next day's header.
    public var sectionSpacing: CGFloat
    /// A horizontal inset applied to both content edges.
    public var horizontalInset: CGFloat
    /// Whether section headers are shown and pinned.
    public var showsHeaders: Bool

    public init(
        sections: [TimelineGridSection],
        columns: Int,
        itemSpacing: CGFloat = 1.5,
        headerHeight: CGFloat = 44,
        sectionSpacing: CGFloat = 12,
        horizontalInset: CGFloat = 0,
        showsHeaders: Bool = true
    ) {
        self.sections = sections
        self.columns = max(1, columns)
        self.itemSpacing = itemSpacing
        self.headerHeight = headerHeight
        self.sectionSpacing = sectionSpacing
        self.horizontalInset = horizontalInset
        self.showsHeaders = showsHeaders
    }

    /// The total number of assets the grid will show — the size the window
    /// store is told up front.
    public var itemCount: Int {
        sections.reduce(0) { $0 + $1.count }
    }

    /// The section title at `index`, or `nil` when out of range.
    public func title(ofSection index: Int) -> String? {
        guard index >= 0, index < sections.count else { return nil }
        return sections[index].title
    }

    /// Resolve the geometry against a container width.
    ///
    /// O(sections) — a few thousand rows for a decade of photos, which is
    /// milliseconds — and independent of the asset count.
    public func layout(forWidth width: CGFloat) -> TimelineLayout {
        TimelineLayout(
            sections: sections.map { TimelineLayout.Section(key: $0.key, count: $0.count) },
            metrics: .fitting(
                width: max(0, width),
                columns: columns,
                itemSpacing: itemSpacing,
                headerHeight: showsHeaders ? headerHeight : 0,
                sectionSpacing: sectionSpacing,
                horizontalInset: horizontalInset
            )
        )
    }

    /// How many items one screenful holds, for a viewport `height` points tall.
    ///
    /// The unit ``AssetWindowStore/Configuration/marginScreens`` is denominated
    /// in. A grid that knows its column count can be exact here, and being exact
    /// is what keeps the read-ahead budget honest across density changes: at
    /// three columns a screenful is a dozen assets, at ten it is a hundred.
    public func viewportItemCount(height: CGFloat, itemSide: CGFloat) -> Int {
        let stride = itemSide + itemSpacing
        guard stride > 0, height > 0 else { return columns }
        let rows = Int((height / stride).rounded(.up))
        return max(columns, rows * columns)
    }
}
