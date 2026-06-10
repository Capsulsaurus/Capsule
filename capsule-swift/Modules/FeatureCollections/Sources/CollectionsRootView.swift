import AssetKit
import CapsuleUI
import FeatureAlbums
import ImagePipeline
import SwiftUI

/// The **Collections** tab — Apple Photos' umbrella over albums, media types,
/// places, and utilities.
///
/// A scrolling home of cover grids and grouped links: My Albums and Media Types
/// (the PhotoKit smart albums) as cover cards, a Utilities group (Recently
/// Deleted, Hidden, Imports, Duplicates), and a More group (Places, People,
/// Memories). Utilities and the deep-ML entries are honest placeholders until
/// later phases (Places lands in 13; Recently Deleted / Hidden / Imports in 14).
public struct CollectionsRootView: View {
    @State private var albums: AlbumsViewModel
    @State private var isCreatingAlbum = false
    @State private var newAlbumName = ""
    let albumProvider: any AlbumProvider
    let assetProvider: any AssetProvider
    let trashProvider: any TrashProvider
    let hiddenStore: HiddenStore
    let thumbnails: any ThumbnailProvider
    let mediaLoader: ViewerMediaLoader

    let gridColumns = [
        GridItem(.flexible(), spacing: CapsuleTheme.Spacing.medium),
        GridItem(.flexible(), spacing: CapsuleTheme.Spacing.medium),
    ]

    public init(
        albumProvider: any AlbumProvider,
        assetProvider: any AssetProvider,
        trashProvider: any TrashProvider,
        hiddenStore: HiddenStore,
        thumbnails: any ThumbnailProvider,
        mediaLoader: ViewerMediaLoader
    ) {
        _albums = State(wrappedValue: AlbumsViewModel(albumProvider: albumProvider))
        self.albumProvider = albumProvider
        self.assetProvider = assetProvider
        self.trashProvider = trashProvider
        self.hiddenStore = hiddenStore
        self.thumbnails = thumbnails
        self.mediaLoader = mediaLoader
    }

    public var body: some View {
        NavigationStack {
            ScrollView {
                LazyVStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xLarge) {
                    if albums.isLoading {
                        ProgressView()
                            .frame(maxWidth: .infinity)
                            .padding(.top, 48)
                    } else {
                        if !albums.userAlbums.isEmpty {
                            albumSection("My Albums", albums.userAlbums)
                        }
                        if !albums.smartAlbums.isEmpty {
                            albumSection("Media Types", albums.smartAlbums)
                        }
                        linkGroup("Utilities", rows: UtilityCategory.allCases.map(AnyCollectionLink.init))
                        linkGroup("More", rows: CollectionCategory.allCases.map(AnyCollectionLink.init))
                    }
                }
                .padding()
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
                switch category {
                case .places:
                    PlacesMapView(
                        assetProvider: assetProvider,
                        albumProvider: albumProvider,
                        thumbnails: thumbnails,
                        mediaLoader: mediaLoader
                    )
                default:
                    CollectionPlaceholderView(
                        title: category.title,
                        systemImage: category.systemImage,
                        message: category.comingSoonMessage
                    )
                }
            }
            .navigationDestination(for: UtilityCategory.self) { utility in
                switch utility {
                case .recentlyDeleted:
                    RecentlyDeletedView(trashProvider: trashProvider)
                case .hidden:
                    HiddenView(
                        assetProvider: assetProvider,
                        hiddenStore: hiddenStore,
                        thumbnails: thumbnails
                    )
                case .imports:
                    ImportsView(
                        assetProvider: assetProvider,
                        albumProvider: albumProvider,
                        thumbnails: thumbnails,
                        mediaLoader: mediaLoader
                    )
                case .duplicates:
                    CollectionPlaceholderView(
                        title: utility.title,
                        systemImage: utility.systemImage,
                        message: utility.comingSoonMessage
                    )
                }
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
}

// MARK: - Sections

private extension CollectionsRootView {
    func albumSection(_ title: String, _ summaries: [AlbumSummary]) -> some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.medium) {
            Text(title).font(.title2.bold())
            LazyVGrid(columns: gridColumns, spacing: CapsuleTheme.Spacing.large) {
                ForEach(summaries) { album in
                    NavigationLink(value: album) {
                        AlbumCoverCard(
                            album: album,
                            albumProvider: albumProvider,
                            assetProvider: assetProvider,
                            thumbnails: thumbnails
                        )
                    }
                    .buttonStyle(.plain)
                }
            }
        }
    }

    func linkGroup(_ title: String, rows: [AnyCollectionLink]) -> some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.medium) {
            Text(title).font(.title2.bold())
            VStack(spacing: 0) {
                ForEach(rows) { row in
                    row.navigationLink
                    if row.id != rows.last?.id {
                        Divider().padding(.leading, 52)
                    }
                }
            }
            .background(
                Color(.secondarySystemBackground),
                in: RoundedRectangle(cornerRadius: CapsuleTheme.Radius.medium)
            )
        }
    }
}

// MARK: - Categories

/// The "More" Collections entries. Placeholders under the pragmatic-parity scope
/// (Places becomes real in Phase 13; People / Memories stay placeholders).
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

/// The Utilities group. Recently Deleted / Hidden / Imports become real in
/// Phase 14; Duplicates stays a placeholder (it needs perceptual hashing).
enum UtilityCategory: String, CaseIterable, Identifiable, Hashable {
    case recentlyDeleted
    case hidden
    case imports
    case duplicates

    var id: String { rawValue }

    var title: String {
        switch self {
        case .recentlyDeleted: "Recently Deleted"
        case .hidden: "Hidden"
        case .imports: "Imports"
        case .duplicates: "Duplicates"
        }
    }

    var systemImage: String {
        switch self {
        case .recentlyDeleted: "trash"
        case .hidden: "eye.slash"
        case .imports: "square.and.arrow.down"
        case .duplicates: "square.on.square"
        }
    }

    var comingSoonMessage: String {
        switch self {
        case .recentlyDeleted: "Restore or permanently remove deleted photos. Coming soon."
        case .hidden: "Photos you've hidden, behind Face ID. Coming soon."
        case .imports: "Photos recently imported into Capsule. Coming soon."
        case .duplicates: "Find and merge duplicate photos. Coming soon."
        }
    }
}

/// A type-erased Collections link row, so Utilities and More can share one
/// grouped-list builder over their different category enums.
struct AnyCollectionLink: Identifiable {
    let id: String
    let navigationLink: AnyView

    init(_ category: CollectionCategory) {
        id = category.id
        navigationLink = AnyView(
            NavigationLink(value: category) {
                CollectionRow(systemImage: category.systemImage, title: category.title)
            }
            .buttonStyle(.plain)
        )
    }

    init(_ utility: UtilityCategory) {
        id = utility.id
        navigationLink = AnyView(
            NavigationLink(value: utility) {
                CollectionRow(systemImage: utility.systemImage, title: utility.title)
            }
            .buttonStyle(.plain)
        )
    }
}
