import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

// MARK: - PortBackedAssetProvider

/// The ``AssetProvider`` the existing screens see, over the new ports.
///
/// This is the whole point of the bridge: `TimelineRootView`, `SearchRootView`,
/// `PlacesMapView`, and the viewer were written against `AssetProvider` and are
/// not being rewritten, while every data path underneath is now a
/// ``LibraryPort`` read and an ``OrganizePort`` write. Nothing here touches
/// PhotoKit, and nothing here can: the type names no system framework.
///
/// ## Two ports, not one
///
/// `AssetProvider` mixes reads and writes; the port layer splits them
/// deliberately, so a faithful adapter needs both. ``LibraryPort`` answers
/// `loadTimeline`, `asset(for:)`, `locations(for:)`, and `changes()`;
/// ``OrganizePort`` performs `setFavorite` and `delete`. Taking both by
/// constructor keeps that visible rather than hiding a second dependency inside
/// a downcast.
public struct PortBackedAssetProvider: AssetProvider, AssetLocationSource {
    /// Rows warmed before a freshly-loaded timeline is handed back. Twenty
    /// windows — enough that the healthy scenario's whole library is real on
    /// first draw, and enough of the huge one that a scroll has somewhere to
    /// start.
    public static let defaultWarmLimit = 4000

    /// Upper bound on ``locations(for:)``. Coordinates live in per-asset
    /// sidecars, so the lookup is one read per asset; a map screen that hands
    /// over a 250 000-asset timeline gets the newest slice rather than a
    /// quarter of a million reads.
    public static let maximumLocationLookups = 5000

    private let library: any LibraryPort
    private let organize: any OrganizePort
    private let query: TimelineQuery
    private let warmLimit: Int

    public init(
        library: any LibraryPort,
        organize: any OrganizePort,
        query: TimelineQuery = .default,
        warmLimit: Int = PortBackedAssetProvider.defaultWarmLimit
    ) {
        self.library = library
        self.organize = organize
        self.query = query
        self.warmLimit = warmLimit
    }

    // MARK: Authorization

    /// Always ``AssetAuthorizationStatus/authorized``, and **nothing is asked**.
    ///
    /// The Capsule library is not the system photo library. It is the app's own
    /// store, read through the app's own port, and there is no third party
    /// whose permission could be sought — so a prompt here would be asking the
    /// user to authorise the app to read its own data. Returning `.authorized`
    /// is not a stub standing in for a real check; it is the complete and
    /// correct answer, and it is what makes "never signed in is a valid mode"
    /// true at launch rather than after a dialog.
    public func authorizationStatus() async -> AssetAuthorizationStatus {
        .authorized
    }

    /// Identical to ``authorizationStatus()``, and just as silent.
    @discardableResult
    public func requestAuthorization() async -> AssetAuthorizationStatus {
        .authorized
    }

    // MARK: Reads

    /// The timeline as a paged snapshot.
    ///
    /// Two aggregate reads and a bounded warm — never a full materialisation,
    /// however large the library is.
    public func loadTimeline() async throws -> any AssetSnapshot {
        let count = try await library.assetCount(matching: query)
        let dayCounts = try await library.dayCounts(matching: query)
        let snapshot = PagedLibrarySnapshot(
            library: library,
            query: query,
            count: count,
            dayCounts: dayCounts
        )
        await snapshot.warm(assetLimit: warmLimit)
        CapsuleLog.assetKit.debug(
            "timeline snapshot: \(count, privacy: .public) assets, \(snapshot.loadedPageCount, privacy: .public) pages warm"
        )
        return snapshot
    }

    public func asset(for id: AssetID) async throws -> Asset? {
        try await library.asset(for: id).map(Asset.init(libraryAsset:))
    }

    /// Capture coordinates, read from each asset's sidecar.
    ///
    /// Provisional identifiers are skipped rather than looked up: they resolve
    /// to nothing by construction, and asking the port about one would be a
    /// pointless round trip per unloaded row.
    public func locations(for ids: [AssetID]) async -> [AssetID: AssetCoordinate] {
        var coordinates: [AssetID: AssetCoordinate] = [:]
        for id in ids.prefix(Self.maximumLocationLookups) {
            guard !Self.isProvisional(id), let gps = try? await library.sidecar(for: id)?.gps else { continue }
            coordinates[id] = AssetCoordinate(latitude: gps.latitude, longitude: gps.longitude)
        }
        return coordinates
    }

    /// Republish the timeline whenever the library says it moved.
    ///
    /// Always a wholesale reload rather than a delta, because ``LibraryChange``
    /// is deliberately not a diff: it names *what kind* of thing changed and
    /// leaves the consumer to re-read the window it cares about. Manufacturing
    /// an `IndexSet` here would mean guessing at a window this adapter does not
    /// own — and a wrong delta animates rows into the wrong places, which is
    /// worse than a reload.
    public nonisolated func changes() -> AsyncStream<AssetChange> {
        AsyncStream { continuation in
            let task = Task {
                for await _ in library.changes() {
                    guard !Task.isCancelled else { break }
                    guard let snapshot = try? await loadTimeline() else { continue }
                    continuation.yield(.reload(snapshot))
                }
                continuation.finish()
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    // MARK: Writes

    /// Set the favourite flag by adding or removing ``Asset/favoriteTag``.
    ///
    /// Adding is guarded on the current state so a repeated favourite does not
    /// pile up duplicate OR-set entries. Removing has to name the **add id**
    /// that introduced each entry — ``OrganizePort`` rejects a remove for an add
    /// this replica never observed, which is the "remove an element you never
    /// added" defence — and the add ids live only on the sidecar, not on the
    /// flattened ``LibraryAsset``. So an un-favourite is a sidecar read followed
    /// by one remove per entry.
    public func setFavorite(_ isFavorite: Bool, for id: AssetID) async throws {
        guard let asset = try await library.asset(for: id) else { return }
        guard isFavorite != asset.isFavorite else { return }
        if isFavorite {
            try await organize.addUserTag(Asset.favoriteTag, to: [id])
            return
        }
        guard let sidecar = try await library.sidecar(for: id) else { return }
        for entry in sidecar.tagsUser.entries where entry.element == Asset.favoriteTag {
            try await organize.removeUserTag(addID: entry.addID, from: id)
        }
    }

    /// Soft-delete into the trash, on the album's own signed retention window.
    ///
    /// `retentionDays: nil` means "whatever the asset's album policy says",
    /// which is the only answer this adapter can honestly give: a deletion
    /// initiated from a grid carries no per-asset retention intent.
    public func delete(_ ids: [AssetID]) async throws {
        let real = ids.filter { !Self.isProvisional($0) }
        guard !real.isEmpty else { return }
        try await organize.moveToTrash(real, retentionDays: nil)
    }

    // MARK: Private

    private static func isProvisional(_ id: AssetID) -> Bool {
        guard case let .managed(uuid) = id else { return false }
        return uuid.hasPrefix(PagedLibrarySnapshot.provisionalIdentifierPrefix)
    }
}
