import AssetKit
import CapsuleUI
import FeatureViewer
import ImagePipeline
import SwiftUI

/// Imports — the photos in the Capsule-managed store (everything brought in via
/// the importer), newest first, opening the viewer on tap.
struct ImportsView: View {
    @State private var assets: [Asset] = []
    @State private var isLoading = true
    @State private var viewerSelection: ImportsViewerSelection?
    private let assetProvider: any AssetProvider
    private let albumProvider: any AlbumProvider
    private let thumbnails: any ThumbnailProvider
    private let mediaLoader: ViewerMediaLoader

    init(
        assetProvider: any AssetProvider,
        albumProvider: any AlbumProvider,
        thumbnails: any ThumbnailProvider,
        mediaLoader: ViewerMediaLoader
    ) {
        self.assetProvider = assetProvider
        self.albumProvider = albumProvider
        self.thumbnails = thumbnails
        self.mediaLoader = mediaLoader
    }

    var body: some View {
        Group {
            if isLoading {
                ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
            } else if assets.isEmpty {
                ContentUnavailableView(
                    "No Imports",
                    systemImage: "square.and.arrow.down",
                    description: Text("Photos imported into Capsule appear here.")
                )
            } else {
                PhotoGridView(
                    sections: [PhotoGridSection(id: "imports", title: "", assets: assets)],
                    columnCount: 5,
                    thumbnails: thumbnails,
                    showsSectionHeaders: false,
                    onSelect: openViewer
                )
                .ignoresSafeArea(edges: .bottom)
            }
        }
        .navigationTitle("Imports")
        .navigationBarTitleDisplayMode(.inline)
        .task { await reload() }
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

    private func reload() async {
        guard let snapshot = try? await assetProvider.loadTimeline() else {
            isLoading = false
            return
        }
        let all = (0 ..< snapshot.count).map { snapshot.asset(at: $0) }
        assets = all.filter(\.isManaged)
        isLoading = false
    }

    private func openViewer(_ asset: Asset) {
        guard let index = assets.firstIndex(of: asset) else { return }
        viewerSelection = ImportsViewerSelection(assets: assets, startIndex: index)
    }
}

private struct ImportsViewerSelection: Identifiable {
    let id = UUID()
    let assets: [Asset]
    let startIndex: Int
}
