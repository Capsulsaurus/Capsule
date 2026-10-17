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
    private let captionStore: (any CaptionStore)?
    private let placeNames: any PlaceNameResolver

    public init(
        album: AlbumSummary,
        albumProvider: any AlbumProvider,
        assetProvider: any AssetProvider,
        thumbnails: any ThumbnailProvider,
        mediaLoader: ViewerMediaLoader,
        captionStore: (any CaptionStore)? = nil,
        placeNames: any PlaceNameResolver = NoPlaceNameResolver()
    ) {
        _model = State(wrappedValue: AlbumDetailViewModel(album: album, albumProvider: albumProvider))
        self.album = album
        self.albumProvider = albumProvider
        self.assetProvider = assetProvider
        self.thumbnails = thumbnails
        self.mediaLoader = mediaLoader
        self.captionStore = captionStore
        self.placeNames = placeNames
    }

    public var body: some View {
        content
            .navigationTitle(album.title)
            .capsuleNavigationBarInline()
            .task { await model.load() }
            .capsuleFullScreenCover(item: $viewerSelection) { selection in
                AssetViewerView(
                    assets: selection.assets,
                    startIndex: selection.startIndex,
                    provider: assetProvider,
                    mediaLoader: mediaLoader,
                    albumProvider: albumProvider,
                    captionStore: captionStore,
                    placeNames: placeNames
                )
            }
    }

    @ViewBuilder
    private var content: some View {
        if model.isLoading {
            ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
        } else if model.assets.isEmpty {
            ContentUnavailableView(
                "app.albums.detail.empty.title",
                systemImage: "photo.on.rectangle",
                description: Text("app.albums.detail.empty.description")
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
