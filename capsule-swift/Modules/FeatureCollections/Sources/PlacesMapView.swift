import AssetKit
import CapsuleFoundation
import CapsuleUI
import FeatureViewer
import ImagePipeline
import MapKit
import SwiftUI

/// The Places map — geotagged photos clustered onto a `Map`. Tapping a cluster
/// opens its photos in a grid, then the viewer.
///
/// Coordinates come from `AssetProvider.locations(for:)`; this round that is the
/// PhotoKit library (system photos carry GPS). Managed-store geotags live only
/// in CBOR sidecars and are a follow-up.
public struct PlacesMapView: View {
    @State private var model: PlacesMapViewModel
    @State private var position: MapCameraPosition = .automatic
    @State private var selectedCluster: PhotoCluster?
    private let assetProvider: any AssetProvider
    private let albumProvider: any AlbumProvider
    private let thumbnails: any ThumbnailProvider
    private let mediaLoader: ViewerMediaLoader

    public init(
        assetProvider: any AssetProvider,
        albumProvider: any AlbumProvider,
        thumbnails: any ThumbnailProvider,
        mediaLoader: ViewerMediaLoader
    ) {
        _model = State(wrappedValue: PlacesMapViewModel(provider: assetProvider))
        self.assetProvider = assetProvider
        self.albumProvider = albumProvider
        self.thumbnails = thumbnails
        self.mediaLoader = mediaLoader
    }

    public var body: some View {
        Map(position: $position) {
            ForEach(model.clusters) { cluster in
                Annotation("", coordinate: cluster.coordinate) {
                    clusterPin(cluster)
                }
            }
        }
        .navigationTitle("Places")
        .navigationBarTitleDisplayMode(.inline)
        .overlay { overlay }
        .task { await model.load() }
        .navigationDestination(item: $selectedCluster) { cluster in
            PlacesClusterGrid(
                assets: cluster.assets,
                assetProvider: assetProvider,
                albumProvider: albumProvider,
                thumbnails: thumbnails,
                mediaLoader: mediaLoader
            )
        }
    }

    private func clusterPin(_ cluster: PhotoCluster) -> some View {
        Button { selectedCluster = cluster } label: {
            Text("\(cluster.assets.count)")
                .font(.caption.bold())
                .foregroundStyle(.white)
                .padding(.horizontal, CapsuleTheme.Spacing.small)
                .padding(.vertical, CapsuleTheme.Spacing.xSmall)
                .background(Color.accentColor, in: Capsule())
                .overlay(Capsule().stroke(.white, lineWidth: 1.5))
        }
        .buttonStyle(.plain)
    }

    @ViewBuilder
    private var overlay: some View {
        if model.isLoading {
            ProgressView()
        } else if model.clusters.isEmpty {
            ContentUnavailableView(
                "No Places",
                systemImage: "mappin.slash",
                description: Text("Photos with location data will appear here.")
            )
        }
    }
}

/// A group of nearby geotagged photos shown as one map pin.
struct PhotoCluster: Identifiable, Hashable {
    let id: String
    let coordinate: CLLocationCoordinate2D
    let assets: [Asset]

    static func == (lhs: PhotoCluster, rhs: PhotoCluster) -> Bool { lhs.id == rhs.id }
    func hash(into hasher: inout Hasher) { hasher.combine(id) }
}

@MainActor
@Observable
final class PlacesMapViewModel {
    private(set) var clusters: [PhotoCluster] = []
    private(set) var isLoading = true
    private let provider: any AssetProvider

    init(provider: any AssetProvider) {
        self.provider = provider
    }

    func load() async {
        guard let snapshot = try? await provider.loadTimeline() else {
            isLoading = false
            return
        }
        let assets = (0 ..< snapshot.count).map { snapshot.asset(at: $0) }
        let coordinates = await provider.locations(for: assets.map(\.id))
        clusters = Self.cluster(assets: assets, coordinates: coordinates)
        isLoading = false
    }

    /// Group located assets by a ~1 km coordinate key (2 decimal places).
    private static func cluster(
        assets: [Asset],
        coordinates: [AssetID: AssetCoordinate]
    ) -> [PhotoCluster] {
        var groups: [String: (coordinate: AssetCoordinate, assets: [Asset])] = [:]
        for asset in assets {
            guard let coordinate = coordinates[asset.id] else { continue }
            let key = String(format: "%.2f,%.2f", coordinate.latitude, coordinate.longitude)
            groups[key, default: (coordinate, [])].assets.append(asset)
        }
        return groups.map { key, value in
            PhotoCluster(
                id: key,
                coordinate: CLLocationCoordinate2D(
                    latitude: value.coordinate.latitude,
                    longitude: value.coordinate.longitude
                ),
                assets: value.assets
            )
        }
    }
}

/// One location's photos, in a grid that opens the viewer on tap.
struct PlacesClusterGrid: View {
    let assets: [Asset]
    let assetProvider: any AssetProvider
    let albumProvider: any AlbumProvider
    let thumbnails: any ThumbnailProvider
    let mediaLoader: ViewerMediaLoader
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
        .navigationTitle("Location")
        .navigationBarTitleDisplayMode(.inline)
        .fullScreenCover(item: $viewerSelection) { selection in
            AssetViewerView(
                assets: selection.assets,
                startIndex: selection.startIndex,
                provider: assetProvider,
                mediaLoader: mediaLoader,
                albumProvider: albumProvider
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
