import AssetKit
import CapsuleUI
import FeatureAlbums
import ImagePipeline
import SwiftUI

/// The **Collections** tab — Apple Photos' umbrella over albums, media types,
/// places, and utilities.
///
/// iOS 26 Photos splits the library into a *Library* timeline and a *Collections*
/// home; Capsule mirrors that. This phase scaffolds the home: real user albums
/// and smart-album "Media Types" sections (which already exist via PhotoKit),
/// plus honest placeholder rows for Places, People, and Memories that later
/// phases replace. Phase 12 turns the albums into a cover grid and fleshes out
/// the sections.
public struct CollectionsRootView: View {
    @State private var albums: AlbumsViewModel
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
        _albums = State(wrappedValue: AlbumsViewModel(albumProvider: albumProvider))
        self.albumProvider = albumProvider
        self.assetProvider = assetProvider
        self.thumbnails = thumbnails
        self.mediaLoader = mediaLoader
    }

    public var body: some View {
        NavigationStack {
            List {
                Section("Browse") {
                    ForEach(CollectionCategory.allCases) { category in
                        NavigationLink(value: category) {
                            Label(category.title, systemImage: category.systemImage)
                        }
                    }
                }
                if !albums.userAlbums.isEmpty {
                    Section("My Albums") {
                        ForEach(albums.userAlbums) { albumRow($0) }
                    }
                }
                if !albums.smartAlbums.isEmpty {
                    Section("Media Types") {
                        ForEach(albums.smartAlbums) { albumRow($0) }
                    }
                }
            }
            .navigationTitle("Collections")
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button { isCreatingAlbum = true } label: {
                        Image(systemName: "plus")
                    }
                    .accessibilityLabel("New Album")
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
            .navigationDestination(for: CollectionCategory.self) { category in
                CollectionPlaceholderView(
                    title: category.title,
                    systemImage: category.systemImage,
                    message: category.comingSoonMessage
                )
            }
        }
        .task { await albums.load() }
        .alert("New Album", isPresented: $isCreatingAlbum) {
            TextField("Album Name", text: $newAlbumName)
            Button("Cancel", role: .cancel) { newAlbumName = "" }
            Button("Create") {
                let name = newAlbumName
                newAlbumName = ""
                Task { await albums.createAlbum(named: name) }
            }
        } message: {
            Text("Name your new Capsule album.")
        }
    }

    private func albumRow(_ album: AlbumSummary) -> some View {
        NavigationLink(value: album) {
            HStack(spacing: CapsuleTheme.Spacing.medium) {
                Image(systemName: album.isUserAlbum
                    ? "rectangle.stack.fill"
                    : "sparkles.rectangle.stack.fill")
                    .font(.title2)
                    .foregroundStyle(.tint)
                    .frame(width: 32)
                VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xxSmall) {
                    Text(album.title)
                    Text("^[\(album.count) Photo](inflect: true)")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
        }
    }
}

/// The non-album Collections sections. Placeholders in this phase; Places
/// becomes real in Phase 13 and People/Memories stay labelled placeholders
/// under the "pragmatic parity" scope (deep ML deferred).
enum CollectionCategory: String, CaseIterable, Identifiable, Hashable {
    case places
    case people
    case memories

    var id: String { rawValue }

    var title: String {
        switch self {
        case .places: "Places"
        case .people: "People & Pets"
        case .memories: "Memories"
        }
    }

    var systemImage: String {
        switch self {
        case .places: "map"
        case .people: "person.2.crop.square.stack"
        case .memories: "sparkles"
        }
    }

    var comingSoonMessage: String {
        switch self {
        case .places: "See where your photos were taken on a map."
        case .people: "People & Pets grouping is coming soon."
        case .memories: "Auto-curated Memories are coming soon."
        }
    }
}
