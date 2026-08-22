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
                            albumSection("ios.albums.section.my_albums", albums.userAlbums)
                        }
                        if !albums.smartAlbums.isEmpty {
                            albumSection("ios.collections.section.media_types", albums.smartAlbums)
                        }
                        linkGroup("ios.collections.section.utilities", rows: UtilityCategory.allCases.map(AnyCollectionLink.init))
                        linkGroup("ios.collections.section.more", rows: CollectionCategory.allCases.map(AnyCollectionLink.init))
                    }
                }
                .padding()
            }
            .navigationTitle("ios.tab.collections")
            .toolbar {
                // `.primaryAction` rather than `.topBarTrailing`: it resolves to
                // the navigation bar's trailing slot on iOS and to the window
                // toolbar on macOS, where the topBar placements do not exist.
                ToolbarItem(placement: .primaryAction) {
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
        .alert("ios.albums.new_album.title", isPresented: $isCreatingAlbum) {
            TextField("ios.albums.new_album.name_field", text: $newAlbumName)
            Button("ios.common.cancel", role: .cancel) { newAlbumName = "" }
            Button("ios.common.create") {
                let name = newAlbumName
                newAlbumName = ""
                Task { await albums.createAlbum(named: name) }
            }
        } message: {
            Text("ios.albums.new_album.message")
        }
    }
}

// MARK: - Sections

private extension CollectionsRootView {
    func albumSection(_ title: LocalizedStringKey, _ summaries: [AlbumSummary]) -> some View {
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

    func linkGroup(_ title: LocalizedStringKey, rows: [AnyCollectionLink]) -> some View {
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
                .fill.secondary,
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
///
/// `@MainActor` because the initializers build SwiftUI views and reach
/// `ButtonStyle.plain`, which is main-actor isolated. Every construction site is
/// already inside a view body, so this costs nothing and removes the isolation
/// warning that would become an error under a stricter language mode.
@MainActor
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
