import AssetKit
import ImagePipeline
import SwiftUI

/// The albums screen — Capsule user albums plus the system smart albums.
public struct AlbumsRootView: View {
    @State private var model: AlbumsViewModel
    @State private var isCreatingAlbum = false
    @State private var newAlbumName = ""
    private let albumProvider: any AlbumProvider
    private let assetProvider: any AssetProvider
    private let thumbnails: any ThumbnailProvider
    private let mediaLoader: ViewerMediaLoader

    public init(
        albumProvider: any AlbumProvider,
        assetProvider: any AssetProvider,
        thumbnails: any ThumbnailProvider,
        mediaLoader: ViewerMediaLoader
    ) {
        _model = State(wrappedValue: AlbumsViewModel(albumProvider: albumProvider))
        self.albumProvider = albumProvider
        self.assetProvider = assetProvider
        self.thumbnails = thumbnails
        self.mediaLoader = mediaLoader
    }

    public var body: some View {
        NavigationStack {
            content
                .navigationTitle("ios.albums.title")
                .toolbar {
                    ToolbarItem(placement: .topBarTrailing) {
                        Button { isCreatingAlbum = true } label: {
                            Image(systemName: "plus")
                        }
                        .accessibilityLabel("ios.albums.new_album.title")
                    }
                }
                .navigationDestination(for: AlbumSummary.self) { album in
                    AlbumDetailView(
                        album: album,
                        albumProvider: albumProvider,
                        assetProvider: assetProvider,
                        thumbnails: thumbnails,
                        mediaLoader: mediaLoader
                    )
                }
        }
        .task { await model.load() }
        .alert("ios.albums.new_album.title", isPresented: $isCreatingAlbum) {
            TextField("ios.albums.new_album.name_field", text: $newAlbumName)
            Button("ios.common.cancel", role: .cancel) { newAlbumName = "" }
            Button("ios.common.create") {
                let name = newAlbumName
                newAlbumName = ""
                Task { await model.createAlbum(named: name) }
            }
        } message: {
            Text("ios.albums.new_album.message")
        }
    }

    @ViewBuilder
    private var content: some View {
        if model.isLoading {
            ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
        } else if model.userAlbums.isEmpty, model.smartAlbums.isEmpty {
            ContentUnavailableView(
                "ios.albums.empty.title",
                systemImage: "rectangle.stack",
                description: Text("ios.albums.empty.description")
            )
        } else {
            List {
                if !model.userAlbums.isEmpty {
                    Section("ios.albums.section.my_albums") {
                        ForEach(model.userAlbums) { albumRow($0) }
                    }
                }
                if !model.smartAlbums.isEmpty {
                    Section("ios.albums.section.smart_albums") {
                        ForEach(model.smartAlbums) { albumRow($0) }
                    }
                }
            }
        }
    }

    private func albumRow(_ album: AlbumSummary) -> some View {
        NavigationLink(value: album) {
            HStack(spacing: 12) {
                Image(systemName: album.isUserAlbum
                    ? "rectangle.stack.fill"
                    : "sparkles.rectangle.stack.fill")
                    .font(.title2)
                    .foregroundStyle(.tint)
                    .frame(width: 32)
                VStack(alignment: .leading, spacing: 2) {
                    Text(album.title)
                    Text("^[\(album.count) Photo](inflect: true)")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
        }
    }
}
