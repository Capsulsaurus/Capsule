import AssetKit
import CapsuleNavigation
import CapsuleUI
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
        content
            .navigationTitle("ios.albums.title")
            // `.primaryAction` rather than `.topBarTrailing`: the topBar
            // placements exist only where there is a navigation bar, while
            // this one resolves to the same trailing slot on iOS and to the
            // window toolbar on macOS.
            .toolbar {
                ToolbarItem(placement: .primaryAction) {
                    Button { isCreatingAlbum = true } label: {
                        Image(systemName: "plus")
                    }
                    .accessibilityLabel("ios.albums.new_album.title")
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
            ScrollView {
                LazyVStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xLarge) {
                    if !model.userAlbums.isEmpty {
                        albumSection("ios.albums.section.my_albums", model.userAlbums)
                    }
                    if !model.smartAlbums.isEmpty {
                        albumSection("ios.albums.section.smart_albums", model.smartAlbums)
                    }
                }
                .padding()
            }
        }
    }

    /// One headed grid of album covers.
    ///
    /// A cover grid rather than a list of rows, because an album is identified
    /// by what is in it long before it is identified by its name — and because
    /// the container/view distinction the design docs insist on is carried by
    /// the tile's own glyph, which a text row has nowhere to put.
    @ViewBuilder
    private func albumSection(_ titleKey: LocalizedStringKey, _ albums: [AlbumSummary]) -> some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.small) {
            Text(titleKey)
                .font(.title3.weight(.semibold))
            LazyVGrid(columns: Self.coverColumns, spacing: CapsuleTheme.Spacing.medium) {
                ForEach(albums) { album in
                    NavigationLink(value: Route.album(album.id)) {
                        AlbumCoverCard(
                            album: album,
                            albumProvider: albumProvider,
                            assetProvider: assetProvider,
                            thumbnails: thumbnails
                        )
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("album.\(album.title)")
                }
            }
        }
    }

    /// Two columns on a phone and as many as fit elsewhere — an adaptive grid
    /// rather than a fixed count, so the iPad and Mac windows earn their width
    /// instead of showing two enormous tiles.
    private static let coverColumns = [
        GridItem(.adaptive(minimum: 150), spacing: CapsuleTheme.Spacing.medium),
    ]
}
