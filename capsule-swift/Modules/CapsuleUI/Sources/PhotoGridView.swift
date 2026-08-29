import AssetKit
import CapsuleFoundation
import ImagePipeline
import SwiftUI

// MARK: - PhotoGridView

/// A high-performance photo grid for SwiftUI, backed by ``PlatformCollectionView``
/// — `UICollectionView` on iOS/iPadOS, `NSCollectionView` on macOS — with a
/// compositional layout and a diffable data source.
///
/// A platform collection is used over `LazyVGrid` for true cell reuse,
/// first-class prefetch/cancel, and pinned section headers — the properties a
/// fast, large-library timeline needs. Every *pixel*, though, is SwiftUI:
/// ``PhotoGridTile``, ``PhotoGridCard``, and ``PhotoGridSectionHeader`` are
/// written once and hosted by both platforms' cells.
///
/// The grid is source-agnostic: it renders ``PhotoGridSection`` values as
/// uniform tiles or representative cards, and is reused by the timeline, album,
/// and aggregation screens.
public struct PhotoGridView: View {
    private let sections: [PhotoGridSection]
    private let style: PhotoGridStyle
    private let thumbnails: any ThumbnailProvider
    private let showsSectionHeaders: Bool
    private let scrollToSectionID: String?
    private let scrollToAsset: Asset?
    private let isSelecting: Bool
    private let selectedIDs: Set<AssetID>
    private let onSelect: (Asset) -> Void
    private let onSelectSection: ((PhotoGridSection) -> Void)?
    private let onZoomLevelChange: ((Bool) -> Void)?
    private let onToggleSelection: ((AssetID) -> Void)?
    private let onLeadingVisibleAsset: ((Asset) -> Void)?
    private let onColumnsChange: ((Int) -> Void)?

    @Environment(\.displayScale) private var displayScale
    /// The state the hosted cells observe. Held here, not passed through the
    /// data source, so a selection change or a resize re-renders the visible
    /// cells without a snapshot round-trip. See ``PhotoGridContext``.
    @State private var context = PhotoGridContext()

    /// The full grid surface: choose a ``PhotoGridStyle``, and — for the
    /// aggregation levels — handle card taps, pinch-to-zoom level changes, and an
    /// optional section to scroll into view after a level switch. Pass
    /// `isSelecting` / `selectedIDs` / `onToggleSelection` to drive multi-select,
    /// and `onLeadingVisibleAsset` to follow where in the library the reader is —
    /// which a grid with no section headers has no other way to say.
    public init(
        sections: [PhotoGridSection],
        style: PhotoGridStyle,
        thumbnails: any ThumbnailProvider,
        showsSectionHeaders: Bool = true,
        scrollToSectionID: String? = nil,
        scrollToAsset: Asset? = nil,
        isSelecting: Bool = false,
        selectedIDs: Set<AssetID> = [],
        onSelect: @escaping (Asset) -> Void,
        onSelectSection: ((PhotoGridSection) -> Void)? = nil,
        onZoomLevelChange: ((Bool) -> Void)? = nil,
        onToggleSelection: ((AssetID) -> Void)? = nil,
        onLeadingVisibleAsset: ((Asset) -> Void)? = nil,
        onColumnsChange: ((Int) -> Void)? = nil
    ) {
        self.sections = sections
        self.style = style
        self.thumbnails = thumbnails
        self.showsSectionHeaders = showsSectionHeaders
        self.scrollToSectionID = scrollToSectionID
        self.scrollToAsset = scrollToAsset
        self.isSelecting = isSelecting
        self.selectedIDs = selectedIDs
        self.onSelect = onSelect
        self.onSelectSection = onSelectSection
        self.onZoomLevelChange = onZoomLevelChange
        self.onToggleSelection = onToggleSelection
        self.onLeadingVisibleAsset = onLeadingVisibleAsset
        self.onColumnsChange = onColumnsChange
    }

    /// Convenience initializer for the common uniform-tile grid.
    public init(
        sections: [PhotoGridSection],
        columnCount: Int,
        thumbnails: any ThumbnailProvider,
        showsSectionHeaders: Bool = true,
        onSelect: @escaping (Asset) -> Void
    ) {
        self.init(
            sections: sections,
            style: .uniform(columns: columnCount),
            thumbnails: thumbnails,
            showsSectionHeaders: showsSectionHeaders,
            onSelect: onSelect
        )
    }

    public var body: some View {
        GeometryReader { proxy in
            let decodeSize = PhotoGridMetrics.decodeSize(
                containerWidth: proxy.size.width,
                style: style,
                displayScale: displayScale
            )
            collection(decodeSize: decodeSize)
                // Pushed through the context rather than captured in the cell
                // closures: already-configured cells never re-run those, but they
                // do observe this.
                .onChange(of: decodeSize, initial: true) { _, size in context.decodeSize = size }
        }
        .onChange(of: isSelecting, initial: true) { _, value in context.isSelecting = value }
        .onChange(of: selectedIDs, initial: true) { _, value in context.selectedIDs = value }
    }

    private func collection(decodeSize: CGSize) -> some View {
        PlatformCollectionView(
            sections: sections.map { PlatformCollectionSection(id: $0.id, items: $0.assets) },
            layout: style.platformLayout(pinnedHeaders: showsSectionHeaders),
            scrollToSectionID: scrollToSectionID,
            scrollToItem: scrollToAsset,
            allowsMultipleSelection: isSelecting,
            onSelect: handleSelection,
            onPrefetch: { assets in
                Task { await thumbnails.beginPrefetching(for: assets, pixelSize: decodeSize) }
            },
            onCancelPrefetch: { assets in
                Task { await thumbnails.cancelPrefetching(for: assets, pixelSize: decodeSize) }
            },
            onMagnify: onZoomLevelChange,
            onLeadingVisibleItem: onLeadingVisibleAsset.map { report in
                { _, asset in report(asset) }
            },
            columns: style.columnCount,
            onColumnsChange: onColumnsChange,
            item: { sectionID, asset in itemContent(sectionID: sectionID, asset: asset) },
            header: { sectionID in
                PhotoGridSectionHeader(title: sectionTitles[sectionID] ?? "")
            }
        )
    }

    @ViewBuilder
    private func itemContent(sectionID: String, asset: Asset) -> some View {
        switch style {
        case .cards:
            PhotoGridCard(
                asset: asset,
                title: sectionTitles[sectionID] ?? "",
                thumbnails: thumbnails,
                context: context
            )
        case .uniform:
            PhotoGridTile(asset: asset, thumbnails: thumbnails, context: context)
        }
    }

    /// Section titles by identifier, so a cell can render its period name
    /// without the collection having to carry section models around.
    private var sectionTitles: [String: String] {
        Dictionary(sections.map { ($0.id, $0.title) }, uniquingKeysWith: { first, _ in first })
    }

    private func handleSelection(sectionID: String, asset: Asset) {
        switch style {
        case .cards:
            guard let section = sections.first(where: { $0.id == sectionID }) else { return }
            onSelectSection?(section)
        case .uniform:
            guard isSelecting else {
                onSelect(asset)
                return
            }
            // Update the observed set first so the tick lands on the same frame
            // as the tap; the owner's `selectedIDs` then arrives and agrees.
            if context.selectedIDs.contains(asset.id) {
                context.selectedIDs.remove(asset.id)
            } else {
                context.selectedIDs.insert(asset.id)
            }
            onToggleSelection?(asset.id)
        }
    }
}
