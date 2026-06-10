import AssetKit
import CapsuleUI
import FeatureViewer
import ImagePipeline
import SwiftUI

/// An album's contents — a flat ``PhotoGridView`` that opens the full-screen
/// viewer on tap.
public struct AlbumDetailView: View {
    @State private var model: AlbumDetailViewModel
    @State private var viewerSelection: AlbumViewerSelection?
    private let album: AlbumSummary
    private let albumProvider: any AlbumProvider
    private let assetProvider: any AssetProvider
    private let thumbnails: any ThumbnailProvider
    private let mediaLoader: ViewerMediaLoader

    public init(
        album: AlbumSummary,
        albumProvider: any AlbumProvider,
        assetProvider: any AssetProvider,
        thumbnails: any ThumbnailProvider,
        mediaLoader: ViewerMediaLoader
    ) {
        _model = State(wrappedValue: AlbumDetailViewModel(album: album, albumProvider: albumProvider))
        self.album = album
        self.albumProvider = albumProvider
        self.assetProvider = assetProvider
        self.thumbnails = thumbnails
        self.mediaLoader = mediaLoader
    }

    public var body: some View {
        content
            .navigationTitle(album.title)
            .navigationBarTitleDisplayMode(.inline)
            .task { await model.load() }
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

    @ViewBuilder
    private var content: some View {
        if model.isLoading {
            ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
        } else if model.assets.isEmpty {
            ContentUnavailableView(
                "Empty Album",
                systemImage: "photo.on.rectangle",
                description: Text("Add photos using a viewer's Add to Album action.")
            )
        } else {
            PhotoGridView(
                sections: [PhotoGridSection(id: "album", title: "", assets: model.assets)],
                columnCount: 5,
                thumbnails: thumbnails,
                showsSectionHeaders: false,
                onSelect: openViewer
            )
            .ignoresSafeArea(edges: .bottom)
        }
    }

    private func openViewer(_ asset: Asset) {
        guard let index = model.assets.firstIndex(of: asset) else { return }
        viewerSelection = AlbumViewerSelection(assets: model.assets, startIndex: index)
    }
}

/// The asset list and entry index handed to a presented viewer.
private struct AlbumViewerSelection: Identifiable {
    let id = UUID()
    let assets: [Asset]
    let startIndex: Int
}
