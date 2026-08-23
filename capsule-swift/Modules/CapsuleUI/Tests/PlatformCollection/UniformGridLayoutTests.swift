#if canImport(UIKit)
    import UIKit
#elseif canImport(AppKit)
    import AppKit
#endif

@testable import CapsuleUI
import Testing

/// Proves the photo grid actually puts `columns` tiles on a row.
///
/// The layout is built from compositional-layout primitives whose sizing rules
/// are easy to get subtly wrong in a way that still compiles, still runs, and
/// still produces a scrollable grid — just one tile wide. That defect shipped:
/// every screenshot of the app showed full-width bands instead of a photo grid,
/// on both iPhone and iPad, and nothing failed.
///
/// This lives under `PlatformCollection/` because it is a test *of* the
/// UIKit/AppKit island and imports it directly, which the `no_platform_ui_import`
/// rule permits only inside that directory.
@MainActor
@Suite("Uniform grid layout geometry")
struct UniformGridLayoutTests {
    private static let width: CGFloat = 400

    /// Resolve the layout and return the item frames of the first row.
    private static func itemFrames(columns: Int, itemCount: Int = 12) -> [CGRect] {
        let layout = PlatformCollectionLayoutBuilder.make(
            .uniformGrid(columns: columns, itemSpacing: 2, pinnedHeaders: false)
        )
        #if canImport(UIKit)
            let view = UICollectionView(
                frame: CGRect(x: 0, y: 0, width: width, height: 800),
                collectionViewLayout: layout
            )
            let source = FixedItemCount(count: itemCount)
            view.dataSource = source
            view.reloadData()
            view.layoutIfNeeded()
            let attributes = layout.layoutAttributesForElements(
                in: CGRect(x: 0, y: 0, width: width, height: 800)
            ) ?? []
            return attributes
                .filter { $0.representedElementCategory == .cell }
                .map(\.frame)
                .sorted { ($0.minY, $0.minX) < ($1.minY, $1.minX) }
        #else
            _ = layout
            return []
        #endif
    }

    #if canImport(UIKit)
        /// The grid must put exactly `columns` tiles across, at roughly
        /// `width / columns` each. This is the assertion whose absence let a
        /// one-tile-wide "grid" ship.
        @Test("a five-column grid puts five tiles on the first row", arguments: [2, 3, 5, 7])
        func rowHoldsRequestedColumns(columns: Int) {
            let frames = Self.itemFrames(columns: columns)
            #expect(!frames.isEmpty)

            let firstRowY = frames[0].minY
            let firstRow = frames.filter { abs($0.minY - firstRowY) < 1 }
            #expect(firstRow.count == columns)

            let expectedSide = Self.width / CGFloat(columns)
            for frame in firstRow {
                #expect(abs(frame.width - expectedSide) <= 3)
            }
        }

        /// A tile is square, so the row is as tall as one column is wide.
        @Test("tiles are square")
        func tilesAreSquare() {
            let frames = Self.itemFrames(columns: 5)
            let first = frames.first
            #expect(first != nil)
            if let first {
                #expect(abs(first.width - first.height) <= 3)
            }
        }

        /// The whole row is used — the last column lands flush with the edge
        /// rather than leaving a ragged margin.
        @Test("the row spans the full width")
        func rowSpansFullWidth() {
            let frames = Self.itemFrames(columns: 5)
            let firstRowY = frames.first?.minY ?? 0
            let firstRow = frames.filter { abs($0.minY - firstRowY) < 1 }
            let spanned = (firstRow.map(\.maxX).max() ?? 0) - (firstRow.map(\.minX).min() ?? 0)
            #expect(abs(spanned - Self.width) <= 4)
        }
    #endif
}

#if canImport(UIKit)
    /// Minimal data source — the layout only needs a count.
    private final class FixedItemCount: NSObject, UICollectionViewDataSource {
        private let count: Int
        init(count: Int) {
            self.count = count
            super.init()
        }

        func collectionView(_: UICollectionView, numberOfItemsInSection _: Int) -> Int { count }

        func collectionView(
            _ collectionView: UICollectionView,
            cellForItemAt indexPath: IndexPath
        ) -> UICollectionViewCell {
            collectionView.register(UICollectionViewCell.self, forCellWithReuseIdentifier: "c")
            return collectionView.dequeueReusableCell(withReuseIdentifier: "c", for: indexPath)
        }
    }
#endif
