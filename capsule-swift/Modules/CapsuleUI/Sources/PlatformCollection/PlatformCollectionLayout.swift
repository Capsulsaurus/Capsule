#if os(macOS)
    import AppKit
#else
    import UIKit
#endif

import CoreGraphics
import Foundation

// MARK: - PlatformCollectionLayout

/// A platform-neutral description of how a ``PlatformCollectionView`` arranges
/// its items.
///
/// Callers describe the *shape they want* rather than building a compositional
/// layout themselves, which is what lets the same grid declaration compile for
/// UIKit and AppKit: the two frameworks share the `NSCollectionLayout*` DSL but
/// not the concrete layout class, the header element kind, or self-sizing
/// support. Everything that differs is resolved in
/// ``PlatformCollectionLayoutBuilder`` — one place, inside the island.
public enum PlatformCollectionLayout: Equatable, Sendable {
    /// `columns` equal square tiles per row, separated by `itemSpacing` points,
    /// optionally with a section header pinned to the top of the viewport.
    case uniformGrid(columns: Int, itemSpacing: CGFloat, pinnedHeaders: Bool)
    /// One full-width item per row, `heightRatio` × the container width tall,
    /// inset by `horizontalInset` / `verticalInset` points.
    case fullWidthRows(heightRatio: CGFloat, horizontalInset: CGFloat, verticalInset: CGFloat)
}

// MARK: - Layout object

// The platform's collection-layout base class. Only the island names it.
#if os(macOS)
    typealias PlatformCollectionLayoutObject = NSCollectionViewLayout
#else
    typealias PlatformCollectionLayoutObject = UICollectionViewLayout
#endif

/// Turns a ``PlatformCollectionLayout`` into the platform's compositional
/// layout object.
///
/// The `NSCollectionLayout*` value types are spelled identically on both
/// platforms, so the section-building maths is written once; only the concrete
/// layout class, the supplementary element kind, and the header's height
/// dimension have to branch.
enum PlatformCollectionLayoutBuilder {
    /// A horizontal group of `count` identical items.
    ///
    /// UIKit renamed this argument to `repeatingSubitem:` and deprecated the old
    /// `subitem:` spelling; AppKit's `NSCollectionLayoutGroup` never gained the
    /// new label. One helper keeps that difference in a single place instead of
    /// forking both call sites.
    private static func horizontalGroup(
        layoutSize: NSCollectionLayoutSize,
        item: NSCollectionLayoutItem,
        count: Int
    ) -> NSCollectionLayoutGroup {
        #if os(macOS)
            return NSCollectionLayoutGroup.horizontal(
                layoutSize: layoutSize,
                subitem: item,
                count: count
            )
        #else
            return NSCollectionLayoutGroup.horizontal(
                layoutSize: layoutSize,
                repeatingSubitem: item,
                count: count
            )
        #endif
    }

    /// The supplementary element kind used for section headers.
    static var headerElementKind: String {
        #if os(macOS)
            NSCollectionView.elementKindSectionHeader
        #else
            UICollectionView.elementKindSectionHeader
        #endif
    }

    /// The height a section header reserves.
    ///
    /// UIKit self-sizes an estimated dimension against the hosted SwiftUI
    /// content; AppKit's compositional layout has no self-sizing pass, so the
    /// Mac gets a fixed height instead of a silently-mis-measured one.
    private static let headerHeight: CGFloat = 44

    static func make(_ layout: PlatformCollectionLayout) -> PlatformCollectionLayoutObject {
        let section = makeSection(layout)
        #if os(macOS)
            return NSCollectionViewCompositionalLayout(section: section)
        #else
            return UICollectionViewCompositionalLayout(section: section)
        #endif
    }

    static func makeSection(_ layout: PlatformCollectionLayout) -> NSCollectionLayoutSection {
        switch layout {
        case let .uniformGrid(columns, itemSpacing, pinnedHeaders):
            makeUniformGrid(columns: columns, itemSpacing: itemSpacing, pinnedHeaders: pinnedHeaders)
        case let .fullWidthRows(heightRatio, horizontalInset, verticalInset):
            makeFullWidthRows(
                heightRatio: heightRatio,
                horizontalInset: horizontalInset,
                verticalInset: verticalInset
            )
        }
    }

    /// `columns` square tiles per row. The gap between tiles is produced by
    /// insetting every item by half the spacing on each edge, so the outer edges
    /// of the grid stay flush with the viewport — the photo-grid look.
    private static func makeUniformGrid(
        columns: Int,
        itemSpacing: CGFloat,
        pinnedHeaders: Bool
    ) -> NSCollectionLayoutSection {
        let columnCount = max(1, columns)
        let inset = itemSpacing / 2
        let item = NSCollectionLayoutItem(layoutSize: NSCollectionLayoutSize(
            widthDimension: .fractionalWidth(1),
            heightDimension: .fractionalHeight(1)
        ))
        item.contentInsets = NSDirectionalEdgeInsets(
            top: inset, leading: inset, bottom: inset, trailing: inset
        )
        let fraction = 1.0 / CGFloat(columnCount)
        let group = horizontalGroup(
            layoutSize: NSCollectionLayoutSize(
                widthDimension: .fractionalWidth(1),
                heightDimension: .fractionalWidth(fraction)
            ),
            item: item,
            count: columnCount
        )
        let section = NSCollectionLayoutSection(group: group)
        if pinnedHeaders {
            section.boundarySupplementaryItems = [makeHeader()]
        }
        return section
    }

    /// One full-width item per row — the Years / Months representative cards.
    private static func makeFullWidthRows(
        heightRatio: CGFloat,
        horizontalInset: CGFloat,
        verticalInset: CGFloat
    ) -> NSCollectionLayoutSection {
        let item = NSCollectionLayoutItem(layoutSize: NSCollectionLayoutSize(
            widthDimension: .fractionalWidth(1),
            heightDimension: .fractionalHeight(1)
        ))
        let group = horizontalGroup(
            layoutSize: NSCollectionLayoutSize(
                widthDimension: .fractionalWidth(1),
                heightDimension: .fractionalWidth(heightRatio)
            ),
            item: item,
            count: 1
        )
        let section = NSCollectionLayoutSection(group: group)
        section.contentInsets = NSDirectionalEdgeInsets(
            top: verticalInset,
            leading: horizontalInset,
            bottom: verticalInset,
            trailing: horizontalInset
        )
        return section
    }

    private static func makeHeader() -> NSCollectionLayoutBoundarySupplementaryItem {
        #if os(macOS)
            let height = NSCollectionLayoutDimension.absolute(headerHeight)
        #else
            let height = NSCollectionLayoutDimension.estimated(headerHeight)
        #endif
        let header = NSCollectionLayoutBoundarySupplementaryItem(
            layoutSize: NSCollectionLayoutSize(widthDimension: .fractionalWidth(1), heightDimension: height),
            elementKind: headerElementKind,
            alignment: .top
        )
        header.pinToVisibleBounds = true
        return header
    }
}
