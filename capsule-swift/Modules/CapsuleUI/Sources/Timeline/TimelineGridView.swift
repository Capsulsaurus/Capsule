import CapsuleDomain
import CapsuleFoundation
import CoreGraphics
import SwiftUI

// MARK: - TimelineAssetFetch

/// Fetches `limit` timeline rows starting at `offset`, in display order.
///
/// A closure rather than a port, on purpose. `CapsuleUI` is the design system:
/// it renders domain state and must never be able to *fetch* one, which is why
/// it does not depend on `CapsulePorts` at all. Handing it a closure keeps the
/// dependency pointing the right way and makes the paging behaviour testable
/// against a counter instead of a library.
public typealias TimelineAssetFetch = @Sendable (_ offset: Int, _ limit: Int) async throws -> [LibraryAsset]

// MARK: - TimelineGridView

/// The virtualized photo grid: ``TimelineLayout`` for geometry,
/// ``AssetWindowStore`` for content, and a collection-view island to put them on
/// screen.
///
/// The two halves never meet directly. The layout knows how many assets each day
/// holds and therefore where every tile goes — including tiles for assets that
/// have not loaded and may never load. The store knows what the few thousand
/// rows near the viewport actually are. The grid's job is to keep them pointed at
/// the same global index range and to render honestly in the gap between them.
///
/// That gap is the normal case, not the exception. On a fast fling most tiles on
/// screen are addressing rows the store has not fetched, so
/// ``TimelineTile`` paints the deepest rung of the degrade ladder it has rather
/// than a spinner or a hole. The scrollbar is still exact, because the content
/// height came from the aggregate and never changes as pages arrive.
///
/// - Note: this is additive. ``PhotoGridView`` still serves every screen that
///   holds its assets in memory; this one serves the ones that cannot.
@MainActor
public struct TimelineGridView: View {
    private let sections: [TimelineGridSection]
    private let columns: Int
    private let images: any TimelineImageSource
    private let showsSectionHeaders: Bool
    private let isSelecting: Bool
    private let selectedIDs: Set<AssetID>
    private let showsCullFlags: Bool
    private let fetch: TimelineAssetFetch
    private let onSelect: (LibraryAsset) -> Void
    private let onToggleSelection: ((AssetID) -> Void)?
    private let onZoomLevelChange: ((Bool) -> Void)?

    @Environment(\.displayScale) private var displayScale

    /// The sliding window of resident rows. Owned here rather than by the caller
    /// so a screen cannot accidentally keep one alive across a query change.
    @State private var store: AssetWindowStore<LibraryAsset>
    /// The current fetch closure, behind one stable reference the store's own
    /// closure can call through. Without it, the store would capture whichever
    /// closure existed when it was created and quietly keep serving the previous
    /// query after the caller changed it.
    @State private var fetchBox: TimelineFetchBox
    /// Presentation state the hosted cells observe. See ``TimelineGridContext``.
    @State private var context = TimelineGridContext()

    /// - Parameters:
    ///   - sections: the **day aggregate** — one row per day, with its count.
    ///     Never the assets: the whole design rests on this being a few thousand
    ///     rows for a decade of photos.
    ///   - columns: tiles per row. Changing it re-lays-out in place.
    ///   - images: where tile pixels come from, rung by rung.
    ///   - showsSectionHeaders: whether day headers are shown and pinned.
    ///   - isSelecting / selectedIDs / onToggleSelection: multi-select mode.
    ///   - showsCullFlags: only true during a culling pass.
    ///   - onSelect: the asset the user activated, outside select mode.
    ///   - onZoomLevelChange: a discrete pinch step: `true` finer, `false`
    ///     coarser.
    ///   - fetch: rows by offset. Called only for pages near the viewport.
    public init(
        sections: [TimelineGridSection],
        columns: Int,
        images: any TimelineImageSource,
        showsSectionHeaders: Bool = true,
        isSelecting: Bool = false,
        selectedIDs: Set<AssetID> = [],
        showsCullFlags: Bool = false,
        onSelect: @escaping (LibraryAsset) -> Void,
        onToggleSelection: ((AssetID) -> Void)? = nil,
        onZoomLevelChange: ((Bool) -> Void)? = nil,
        fetch: @escaping TimelineAssetFetch
    ) {
        self.sections = sections
        self.columns = max(1, columns)
        self.images = images
        self.showsSectionHeaders = showsSectionHeaders
        self.isSelecting = isSelecting
        self.selectedIDs = selectedIDs
        self.showsCullFlags = showsCullFlags
        self.onSelect = onSelect
        self.onToggleSelection = onToggleSelection
        self.onZoomLevelChange = onZoomLevelChange
        self.fetch = fetch

        let box = TimelineFetchBox(fetch: fetch)
        _fetchBox = State(initialValue: box)
        _store = State(initialValue: AssetWindowStore<LibraryAsset>(
            totalCount: sections.reduce(0) { $0 + $1.count }
        ) { offset, limit in
            try await box.load(offset: offset, limit: limit)
        })
    }

    // MARK: Body

    public var body: some View {
        content
            .onChange(of: sections, initial: true) { _, value in adopt(value) }
            .onChange(of: isSelecting, initial: true) { _, value in context.isSelecting = value }
            .onChange(of: selectedIDs, initial: true) { _, value in context.selectedIDs = value }
            .onChange(of: showsCullFlags, initial: true) { _, value in context.showsCullFlags = value }
            .onDisappear { store.cancelOutstandingFetches() }
    }

    @ViewBuilder
    private var content: some View {
        if sections.isEmpty {
            emptyState
        } else {
            grid
        }
    }

    private var grid: some View {
        // The only thing the container width is needed for here is the decode
        // size; the *layout* resolves its own width inside the island, so a
        // resize never has to round-trip through SwiftUI to move a tile.
        GeometryReader { proxy in
            collection
                .onChange(of: decodeSize(containerWidth: proxy.size.width), initial: true) { _, value in
                    context.decodeSize = value
                }
        }
    }

    private var collection: some View {
        TimelineCollectionView(
            geometry: geometry,
            allowsMultipleSelection: isSelecting,
            onSelect: handleSelection,
            onVisibleRangeChange: handleVisibleRange,
            onPrefetch: { handlePrefetch($0, cancel: false) },
            onCancelPrefetch: { handlePrefetch($0, cancel: true) },
            onMagnify: onZoomLevelChange,
            itemContent: tile(at:),
            headerContent: header(ofSection:)
        )
    }

    private var emptyState: some View {
        ContentUnavailableView {
            Label("app.timeline.empty.title", systemImage: "photo.on.rectangle")
        } description: {
            Text("app.timeline.empty.description")
        }
    }

    // MARK: Cells

    private func tile(at globalIndex: Int) -> some View {
        TimelineTile(globalIndex: globalIndex, store: store, context: context, images: images)
    }

    @ViewBuilder
    private func header(ofSection section: Int) -> some View {
        if let title = geometry.title(ofSection: section) {
            PhotoGridSectionHeader(title: title)
        }
    }

    // MARK: Geometry

    private var geometry: TimelineGridGeometry {
        TimelineGridGeometry(
            sections: sections,
            columns: columns,
            itemSpacing: PhotoGridMetrics.tileSpacing,
            showsHeaders: showsSectionHeaders
        )
    }

    private func decodeSize(containerWidth: CGFloat) -> CGSize {
        PhotoGridMetrics.decodeSize(
            containerWidth: containerWidth,
            style: .uniform(columns: columns),
            displayScale: displayScale
        )
    }

    // MARK: Callbacks

    /// Take on a new aggregate.
    ///
    /// Rows are addressed by *offset*, so any change to the day counts means
    /// index 400 is no longer the same asset. Keeping the resident pages would
    /// show the right number of the wrong photos, which is why both branches
    /// throw them away.
    private func adopt(_ sections: [TimelineGridSection]) {
        fetchBox.fetch = fetch
        let total = sections.reduce(0) { $0 + $1.count }
        if total == store.totalCount {
            store.invalidate()
        } else {
            store.reset(totalCount: total)
        }
    }

    private func handleVisibleRange(_ range: Range<Int>, viewportItemCount: Int) {
        store.setVisibleRange(range, viewportItemCount: viewportItemCount)
    }

    private func handleSelection(_ globalIndex: Int) {
        guard let asset = store.element(at: globalIndex) else { return }
        guard isSelecting else {
            onSelect(asset)
            return
        }
        // Update the observed set first so the tick lands on the same frame as
        // the tap; the owner's `selectedIDs` then arrives and agrees.
        if context.selectedIDs.contains(asset.id) {
            context.selectedIDs.remove(asset.id)
        } else {
            context.selectedIDs.insert(asset.id)
        }
        onToggleSelection?(asset.id)
    }

    /// Warm or drop thumbnail caches for rows about to appear.
    ///
    /// Only *resident* rows can be prefetched — a row the store has not fetched
    /// has no identifier to warm. That is the correct behaviour rather than a
    /// limitation: the store's own read-ahead margin is what puts the row in
    /// hand, and the image prefetch then follows it one step behind.
    private func handlePrefetch(_ indices: [Int], cancel: Bool) {
        let size = context.decodeSize
        guard size.width > 0, size.height > 0 else { return }
        let assets = indices.compactMap { store.element(at: $0) }
        guard !assets.isEmpty else { return }
        let images = images
        Task {
            if cancel {
                await images.cancelPrefetching(assets, pixelSize: size)
            } else {
                await images.beginPrefetching(assets, pixelSize: size)
            }
        }
    }
}

// MARK: - TimelineFetchBox

/// One stable reference the store's fetch closure calls through.
///
/// ``AssetWindowStore`` captures its fetch closure for its whole lifetime, which
/// is correct for the store and wrong for a SwiftUI view: the view is handed a
/// fresh closure on every render, and the one captured at `init` may close over
/// a filter or a port the caller has since replaced. Indirecting through a box
/// lets the store keep one closure forever while that closure always calls the
/// current one.
@MainActor
private final class TimelineFetchBox {
    var fetch: TimelineAssetFetch

    init(fetch: @escaping TimelineAssetFetch) {
        self.fetch = fetch
    }

    func load(offset: Int, limit: Int) async throws -> [LibraryAsset] {
        try await fetch(offset, limit)
    }
}
