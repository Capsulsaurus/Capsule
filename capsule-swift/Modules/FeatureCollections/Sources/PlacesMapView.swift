import AssetKit
import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import CapsuleUI
import FeatureViewer
import ImagePipeline
import MapKit
import SwiftUI

/// The Places map — geotagged photos clustered onto a `Map`.
///
/// ## Served by the port, not by the timeline
///
/// This screen used to load the *entire* timeline into memory and bucket it
/// client-side at a fixed ~1 km grid. That is a hang on a real library — the
/// `hugeLibrary` scenario is 250 000 assets — and it ignored the `PlacesPort`
/// that already existed. Clustering is now the port's job, which is also where
/// it belongs: the granularity can follow the zoom, and a cluster's photos are
/// fetched a page at a time instead of being held for every pin at once.
///
/// ## Pointer and touch reach the same preview
///
/// Hovering a pin on macOS or with an iPad pointer fans out its most recent
/// photos in place. Touch has no hover, so a tap does it instead and a second
/// tap — on the fan — opens the place. That keeps the phone, which is the
/// priority platform, from being the one that loses the feature.
public struct PlacesMapView: View {
    @State private var model: PlacesMapViewModel
    @State private var position: MapCameraPosition = .automatic
    @State private var openedCluster: PlaceClusterPreview?
    /// Which pin is showing its fan. At most one, so opening a second closes
    /// the first without either pin having to know about the other.
    @State private var previewedClusterID: String?
    private let assetProvider: any AssetProvider
    private let albumProvider: any AlbumProvider
    private let thumbnails: any ThumbnailProvider
    private let mediaLoader: ViewerMediaLoader
    private let captionStore: (any CaptionStore)?

    public init(
        places: any PlacesPort,
        assetProvider: any AssetProvider,
        albumProvider: any AlbumProvider,
        thumbnails: any ThumbnailProvider,
        mediaLoader: ViewerMediaLoader,
        captionStore: (any CaptionStore)? = nil
    ) {
        _model = State(wrappedValue: PlacesMapViewModel(places: places, assets: assetProvider))
        self.assetProvider = assetProvider
        self.albumProvider = albumProvider
        self.thumbnails = thumbnails
        self.mediaLoader = mediaLoader
        self.captionStore = captionStore
    }

    public var body: some View {
        Map(position: $position) {
            ForEach(model.clusters) { cluster in
                Annotation("", coordinate: cluster.coordinate) {
                    clusterPin(cluster)
                }
            }
        }
        .navigationTitle("app.places.title")
        .capsuleNavigationBarInline()
        .overlay { overlay }
        .task { await model.load() }
        .navigationDestination(item: $openedCluster) { cluster in
            PlacesClusterGrid(
                assets: cluster.assets,
                assetProvider: assetProvider,
                albumProvider: albumProvider,
                thumbnails: thumbnails,
                mediaLoader: mediaLoader,
                captionStore: captionStore
            )
        }
    }

    // MARK: The pin

    private func clusterPin(_ cluster: PhotoCluster) -> some View {
        ClusterPin(
            cluster: cluster,
            previewedClusterID: $previewedClusterID,
            preview: { model.preview(for: cluster.id) },
            loadPreview: { await model.loadPreview(for: cluster.id) },
            open: { open(cluster) },
            thumbnails: thumbnails
        )
    }

    private func open(_ cluster: PhotoCluster) {
        Task {
            guard let opened = await model.openCluster(cluster) else { return }
            openedCluster = opened
        }
    }

    @ViewBuilder
    private var overlay: some View {
        if model.isLoading {
            ProgressView()
        } else if model.clusters.isEmpty {
            ContentUnavailableView(
                "app.places.empty.title",
                systemImage: "mappin.slash",
                description: Text("app.places.empty.description")
            )
        }
    }
}

// MARK: - PhotoCluster

/// One map pin: a `PlaceCluster` with its coordinate already in MapKit terms.
///
/// A view-layer type because `PlaceCluster` deliberately carries no MapKit —
/// the domain layer stays on the clean side of the platform boundary, so the
/// conversion happens here, once.
struct PhotoCluster: Identifiable, Hashable {
    let id: String
    let coordinate: CLLocationCoordinate2D
    let assetCount: Int
    /// Whether the stored datum disagrees with the map's, making the pin
    /// approximate. Surfaced rather than silently converted: the inverse is
    /// lossy, and an unmarked pin is a pin in the wrong street.
    let isApproximate: Bool

    init(_ cluster: PlaceCluster) {
        id = cluster.id
        coordinate = CLLocationCoordinate2D(
            latitude: cluster.centroid.latitude,
            longitude: cluster.centroid.longitude
        )
        assetCount = cluster.assetCount
        isApproximate = cluster.centroid.datum.displaysAsApproximate
    }

    static func == (lhs: PhotoCluster, rhs: PhotoCluster) -> Bool { lhs.id == rhs.id }
    func hash(into hasher: inout Hasher) { hasher.combine(id) }
}

// MARK: - PlaceClusterPreview

/// A place, opened: its assets resolved into the type the grid and viewer take.
struct PlaceClusterPreview: Identifiable, Hashable {
    let id: String
    let assets: [Asset]

    static func == (lhs: PlaceClusterPreview, rhs: PlaceClusterPreview) -> Bool { lhs.id == rhs.id }
    func hash(into hasher: inout Hasher) { hasher.combine(id) }
}

// MARK: - PlacesMapViewModel

@MainActor
@Observable
final class PlacesMapViewModel {
    /// How many assets a fan shows. The port is asked for exactly this many.
    static let previewCount = FannedAssetStack.maximumCards
    /// The cap on a place opened into the grid. A page rather than everything:
    /// a cluster at a coarse granularity can hold a large fraction of a
    /// library, and the grid does not need it all to draw its first screen.
    static let openPageSize = 500
    /// The clustering granularity, until the map reports its camera.
    ///
    /// Fixed for now, and honestly so: the port takes a granularity precisely
    /// so the pins can coarsen as the map zooms out, and wiring that to
    /// `onMapCameraChange` is the follow-up this refactor makes possible rather
    /// than one it completes.
    static let granularity = 6

    private(set) var clusters: [PhotoCluster] = []
    private(set) var isLoading = true

    private let places: any PlacesPort
    private let assets: any AssetProvider
    /// Fan contents, by cluster id. Kept because a pointer sweeping across a
    /// map re-enters pins constantly, and re-fetching on every entry would
    /// make the fan flicker.
    private var previews: [String: [Asset]] = [:]

    init(places: any PlacesPort, assets: any AssetProvider) {
        self.places = places
        self.assets = assets
    }

    func load() async {
        defer { isLoading = false }
        // `try?` flattens the port's `MapRegion?` into one optional, so this
        // guard covers both "nothing is geotagged" and "the read failed". They
        // land in the same place — an empty map under the empty state — which
        // is the honest rendering of each.
        guard let region = try? await places.boundingRegion() else { return }
        guard let found = try? await places.clusters(in: region, granularity: Self.granularity) else {
            return
        }
        clusters = found.map(PhotoCluster.init)
    }

    /// The fan for one pin, or nothing while it is still loading.
    func preview(for clusterID: String) -> [Asset] { previews[clusterID] ?? [] }

    func loadPreview(for clusterID: String) async {
        guard previews[clusterID] == nil else { return }
        let page = try? await places.assets(in: clusterID, offset: 0, limit: Self.previewCount)
        previews[clusterID] = await resolve(page?.items ?? [])
    }

    /// Resolve a whole place for the grid.
    func openCluster(_ cluster: PhotoCluster) async -> PlaceClusterPreview? {
        let page = try? await places.assets(in: cluster.id, offset: 0, limit: Self.openPageSize)
        let resolved = await resolve(page?.items ?? [])
        guard !resolved.isEmpty else { return nil }
        return PlaceClusterPreview(id: cluster.id, assets: resolved)
    }

    /// `LibraryAsset` ids back into the `Asset` the grid and viewer take.
    ///
    /// The seam between the port's domain type and the older provider surface
    /// the viewer still consumes. It is the one place that conversion happens,
    /// so retiring it later is a change to this function rather than to a
    /// screen.
    private func resolve(_ items: [LibraryAsset]) async -> [Asset] {
        var out: [Asset] = []
        out.reserveCapacity(items.count)
        for item in items {
            if let asset = try? await assets.asset(for: item.id) {
                out.append(asset)
            }
        }
        return out
    }
}

/// One location's photos, in a grid that opens the viewer on tap.
struct PlacesClusterGrid: View {
    let assets: [Asset]
    let assetProvider: any AssetProvider
    let albumProvider: any AlbumProvider
    let thumbnails: any ThumbnailProvider
    let mediaLoader: ViewerMediaLoader
    let captionStore: (any CaptionStore)?
    @State private var viewerSelection: ClusterViewerSelection?

    var body: some View {
        PhotoGridView(
            sections: [PhotoGridSection(id: "place", title: "", assets: assets)],
            columnCount: 5,
            thumbnails: thumbnails,
            showsSectionHeaders: false,
            onSelect: openViewer
        )
        .ignoresSafeArea(edges: .bottom)
        .navigationTitle("app.common.location")
        .capsuleNavigationBarInline()
        .capsuleFullScreenCover(item: $viewerSelection) { selection in
            AssetViewerView(
                assets: selection.assets,
                startIndex: selection.startIndex,
                provider: assetProvider,
                mediaLoader: mediaLoader,
                albumProvider: albumProvider,
                captionStore: captionStore
            )
        }
    }

    private func openViewer(_ asset: Asset) {
        guard let index = assets.firstIndex(of: asset) else { return }
        viewerSelection = ClusterViewerSelection(assets: assets, startIndex: index)
    }
}

private struct ClusterViewerSelection: Identifiable {
    let id = UUID()
    let assets: [Asset]
    let startIndex: Int
}

// MARK: - ClusterPin

/// One map pin, and the fan it opens.
///
/// A view of its own rather than a `@ViewBuilder` on the map, because a
/// `Map`'s `Annotation` content does not reliably re-render when state declared
/// on the *map's* view changes — the pin kept its first appearance and the fan
/// never showed. Owning the interaction here means the thing that changes and
/// the thing that redraws are the same view.
private struct ClusterPin: View {
    let cluster: PhotoCluster
    /// Shared so only one pin fans at a time; local `@State` per pin would let
    /// every pin on screen open at once.
    @Binding var previewedClusterID: String?
    let preview: () -> [Asset]
    let loadPreview: @Sendable () async -> Void
    let open: () -> Void
    let thumbnails: any ThumbnailProvider

    private var isPreviewing: Bool { previewedClusterID == cluster.id }

    var body: some View {
        VStack(spacing: CapsuleTheme.Spacing.xSmall) {
            if isPreviewing {
                fan
            }
            badge
        }
        .animation(.snappy(duration: 0.22), value: isPreviewing)
        // Pointer platforms get the fan on hover, with no tap and nothing to
        // dismiss. Inert on touch, which is why the badge carries the same job.
        .onHover { isInside in
            if isInside {
                previewedClusterID = cluster.id
            } else if isPreviewing {
                previewedClusterID = nil
            }
        }
        .task(id: isPreviewing) {
            guard isPreviewing else { return }
            await loadPreview()
        }
    }

    private var badge: some View {
        Button {
            // Touch: the first tap fans the cards, so a place can be inspected
            // without leaving the map, and the second opens it.
            if isPreviewing {
                open()
            } else {
                previewedClusterID = cluster.id
            }
        } label: {
            HStack(spacing: CapsuleTheme.Spacing.xxSmall) {
                if cluster.isApproximate {
                    Image(systemName: "questionmark.circle.fill")
                        .font(.caption2)
                        .accessibilityLabel("app.places.approximate")
                }
                Text(verbatim: "\(cluster.assetCount)")
                    .font(.caption.bold())
                    .monospacedDigit()
            }
            .foregroundStyle(.white)
            .padding(.horizontal, CapsuleTheme.Spacing.small)
            .padding(.vertical, CapsuleTheme.Spacing.xSmall)
            .background(Color.accentColor, in: Capsule())
            .overlay(Capsule().stroke(.white, lineWidth: CapsuleTheme.Stroke.hairline))
        }
        .buttonStyle(.plain)
        // Not unique — one per pin, and a sweep only needs to reach *a* pin.
        .accessibilityIdentifier("place.pin")
    }

    /// Shown the moment the pin is picked rather than when its photos arrive:
    /// the cards draw their own empty state, so appearing at once costs nothing
    /// and the tap is acknowledged immediately.
    private var fan: some View {
        Button(action: open) {
            FannedAssetStack(
                assets: preview(),
                placeholderCount: min(cluster.assetCount, FannedAssetStack.maximumCards),
                side: 58,
                thumbnails: thumbnails
            )
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("place.preview")
        .accessibilityLabel("app.places.preview.open")
        .transition(.scale(scale: 0.7, anchor: .bottom).combined(with: .opacity))
    }
}
