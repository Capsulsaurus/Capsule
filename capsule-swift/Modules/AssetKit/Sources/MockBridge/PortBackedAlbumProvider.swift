import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

// MARK: - PortBackedAlbumProvider

/// The ``AlbumProvider`` the albums and collections screens see, over
/// ``AlbumPort``.
///
/// ## Container albums only
///
/// ``AlbumPort`` exposes two different things the UI both calls "albums", and
/// this adapter surfaces only one of them. ``ContainerAlbum`` owns assets and
/// holds keys; ``ViewAlbum`` — All, Trash, Hidden, Quarantine, and the user's
/// smart albums — is a derived, key-free *query*, and the domain is emphatic
/// that a view is never a destination. `AlbumProvider` has a single
/// ``AlbumProvider/addAsset(_:to:)`` with no way to refuse, so listing views
/// here would offer the user a destination the system cannot accept. Trash and
/// Hidden already have their own screens; smart albums need
/// ``SmartAlbumPort/evaluate(_:offset:limit:)`` rather than a container read,
/// and belong to a screen that can call it.
///
/// ## Two ports again
///
/// ``AlbumPort`` has no asset-listing method at all — deliberately, since
/// membership is a timeline query with an album facet rather than a stored
/// list. So ``assets(in:)`` reads ``LibraryPort`` with
/// ``TimelineQuery/albumID`` set, and both ports arrive by constructor.
public struct PortBackedAlbumProvider: AlbumProvider {
    /// Upper bound on ``assets(in:)``. The protocol returns a materialised
    /// array, so the only honest bound is an explicit one: a screen that needs
    /// more than this many rows needs a paged grid, not a longer array.
    public static let maximumAlbumAssets = 5000

    /// The policy a newly created album is fixed to.
    ///
    /// `AlbumProvider.createUserAlbum(named:)` carries no policy, and an album's
    /// policy is fixed at creation and changeable only through an upgrade
    /// ceremony — so this adapter has to choose one. Full history, the domain's
    /// default retention window, and the protocol version this build writes.
    /// The pin must track the build's wire version; it is stated here rather
    /// than defaulted invisibly so the drift is greppable.
    public static let defaultPolicy = AlbumPolicy(
        historyPolicy: .full,
        retentionDays: TrashEntry.defaultRetentionDays,
        protocolVersion: "2026-05-01"
    )

    /// Catalog key for the nameless default album's display name.
    ///
    /// The default album genuinely has **no name** — that is how the domain
    /// models "the album an unfiled import lands in", and giving it a stored
    /// name would make it look renameable. `AlbumSummary.title` is a plain
    /// `String` rendered with `Text(_: String)`, so the localized value has to
    /// be resolved here rather than deferred to a `LocalizedStringKey`.
    public static let defaultAlbumTitleKey = "ios.albums.default.name"

    private let albums: any AlbumPort
    private let library: any LibraryPort

    public init(albums: any AlbumPort, library: any LibraryPort) {
        self.albums = albums
        self.library = library
    }

    public func loadAlbums() async -> [AlbumSummary] {
        do {
            return try await albums.containerAlbums().map(Self.summary(of:))
        } catch {
            CapsuleLog.assetKit.error(
                "album list failed: \(String(describing: error), privacy: .public)"
            )
            return []
        }
    }

    public func assets(in albumID: AlbumID) async throws -> [Asset] {
        guard try await albums.containerAlbum(albumID) != nil else { throw AlbumError.notFound }
        let query = TimelineQuery(albumID: albumID)
        let page = try await library.assets(matching: query, offset: 0, limit: Self.maximumAlbumAssets)
        return page.items.map(Asset.init(libraryAsset:))
    }

    public func createUserAlbum(named name: String) async throws {
        _ = try await albums.createAlbum(name: name, policy: Self.defaultPolicy)
    }

    /// Add an asset to an album — a **move**, because an asset lives in exactly
    /// one container.
    ///
    /// `AlbumProvider` says "add", which is the PhotoKit vocabulary where an
    /// asset can be in many albums at once. Capsule's containers are the
    /// cryptographic unit and membership is exclusive, so the faithful
    /// translation is ``AlbumPort/move(_:to:)`` — idempotent, so a retry after a
    /// dropped connection finds the target state already in place.
    public func addAsset(_ assetID: AssetID, to albumID: AlbumID) async throws {
        guard try await albums.containerAlbum(albumID) != nil else { throw AlbumError.notFound }
        try await albums.move([assetID], to: albumID)
    }

    public nonisolated func changes() -> AsyncStream<Void> {
        albums.changes()
    }

    // MARK: Private

    private static func summary(of album: ContainerAlbum) -> AlbumSummary {
        AlbumSummary(
            id: album.id,
            title: album.name ?? String(localized: "ios.albums.default.name", bundle: .main),
            count: album.count,
            coverAssetID: album.coverAssetID
        )
    }
}
