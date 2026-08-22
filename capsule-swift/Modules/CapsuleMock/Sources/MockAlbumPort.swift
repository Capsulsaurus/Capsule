import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

// MARK: - AlbumPort

extension MockLibraryStore: AlbumPort {
    public func containerAlbums() async throws -> [ContainerAlbum] {
        albumList
    }

    public func containerAlbum(_ identifier: AlbumID) async throws -> ContainerAlbum? {
        container(identifier)
    }

    /// The derived, key-free views.
    ///
    /// A view holds no AMK, owns no assets, and is **not** an access-control
    /// boundary — which is why it can never be an import destination, and why
    /// its count is optional: membership is a query, not a stored number.
    public func viewAlbums() async throws -> [ViewAlbum] {
        let engine = self.engine
        var views: [ViewAlbum] = [
            ViewAlbum(
                id: .smart(localIdentifier: "view.all"),
                kind: .system(.all),
                count: engine.count(matching: .default),
                coverAssetID: library.assetCount > 0 ? library.identifier(at: 0) : nil
            ),
            ViewAlbum(
                id: .smart(localIdentifier: "view.trash"),
                kind: .system(.trash),
                count: engine.count(matching: .trash)
            ),
            ViewAlbum(
                id: .smart(localIdentifier: "view.hidden"),
                kind: .system(.hidden),
                count: engine.count(matching: .hidden)
            ),
            ViewAlbum(
                id: .smart(localIdentifier: "view.quarantine"),
                kind: .system(.quarantine),
                count: configuration.quarantineSurfaces.count
            ),
        ]
        // A definition ahead of this build's grammar is listed with no count:
        // it is preserved verbatim and never evaluated, so there is no honest
        // number to show.
        views.append(contentsOf: smartAlbumList.map { definition in
            ViewAlbum(
                id: .smart(localIdentifier: definition.smartAlbumID.rawValue),
                kind: .smart(definition.smartAlbumID),
                count: evaluableCount(definition)
            )
        })
        return views
    }

    /// Resolve the destination an unfiled import lands in.
    ///
    /// **Always a container**, never a view, and the rule that fired is recorded
    /// so a surprising destination is explainable after the fact rather than
    /// only reproducible.
    public func resolveDefaultAlbum(
        for scope: ImportScope?
    ) async throws -> (album: ContainerAlbum, rule: ImportPlan.DestinationRule) {
        if let scope, let mapped = recordedScopeOverrides[scope], let album = container(mapped) {
            return (album, .scopeOverride)
        }
        guard let album = container(designatedDefaultAlbumID) ?? albumList.first else {
            throw CapsuleError(code: .albumNotAvailable, detail: "CapsuleMock: no container album exists")
        }
        return (album, album.id == designatedDefaultAlbumID ? .ownerDefaultPointer : .derivedDefaultAlbum)
    }

    /// Create a container album.
    ///
    /// Its policy is fixed here and afterwards changeable only through an
    /// upgrade ceremony, so the mock refuses a policy naming a value it cannot
    /// write rather than coercing it — an old client authoring a value it cannot
    /// name is the strip-and-resign hazard the closed enums exist to stop.
    public func createAlbum(name: String, policy: AlbumPolicy) async throws -> ContainerAlbum {
        try policy.historyPolicy.requireWritable()
        let ordinal = 500_000 + albumList.count
        let album = ContainerAlbum(
            id: MockIdentifiers.albumID(seed: configuration.seed, ordinal: ordinal),
            name: name,
            count: 0,
            epoch: 1,
            policy: policy,
            members: [AlbumMember(handle: MockSidecarFactory.ownerHandle, role: .admin)]
        )
        insertContainer(album)
        await albumChanges.send(())
        return album
    }

    public func renameAlbum(_ identifier: AlbumID, to name: String) async throws {
        updateContainer(identifier) { $0.name = name }
        await albumChanges.send(())
    }

    public func setCoverAsset(_ assetID: AssetID?, for albumID: AlbumID) async throws {
        updateContainer(albumID) { $0.coverAssetID = assetID }
        await albumChanges.send(())
    }

    /// Delete an album.
    ///
    /// **Refused for the currently-designated default.** The user must repoint
    /// first, so an import always has somewhere to land — a library with no
    /// default album is a library where the next photograph has nowhere to go.
    public func deleteAlbum(_ identifier: AlbumID) async throws {
        guard identifier != designatedDefaultAlbumID else {
            throw CapsuleError(
                code: .uploadInvalidAction,
                detail: "CapsuleMock: the designated default album cannot be deleted"
            )
        }
        removeContainer(identifier)
        await albumChanges.send(())
        await announceReload()
    }

    /// Move assets into a container album.
    ///
    /// Idempotent: replaying it finds the target state already in place and
    /// no-ops, so a retry after a dropped connection is safe rather than a
    /// second move.
    public func move(_ assetIDs: [AssetID], to albumID: AlbumID) async throws {
        guard container(albumID) != nil else {
            throw CapsuleError(code: .albumNotAvailable, detail: "CapsuleMock: unknown album")
        }
        await mutate(assetIDs) { $0.albumID = albumID }
        await albumChanges.send(())
    }

    public nonisolated func changes() -> AsyncStream<Void> {
        albumChanges.subscribe()
    }

    /// How many assets a smart album currently selects, when saying is cheap.
    ///
    /// `nil` for a definition this build must not evaluate, and `nil` again once
    /// the library is large enough that counting would mean a full evaluation
    /// per listed view. A view's count is a query rather than a stored number,
    /// and the domain says so — a UI renders a count-less row rather than
    /// blocking a screen on four full scans.
    private func evaluableCount(_ definition: SmartAlbumDefinition) -> Int? {
        guard definition.isEvaluable, library.assetCount <= Self.smartAlbumCountingCeiling else { return nil }
        return evaluate(definition, offset: 0, limit: library.assetCount).items.count
    }

    /// Above this many assets, smart-album counts are reported as unknown.
    static var smartAlbumCountingCeiling: Int { 20_000 }
}
