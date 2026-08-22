import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

// MARK: - FederationPort

extension MockFederationStore: FederationPort {
    public func aggregatedAlbums() async throws -> [AggregatedAlbum] {
        groupList
    }

    public func aggregatedAlbum(_ identifier: AlbumGroupID) async throws -> AggregatedAlbum? {
        group(identifier)
    }

    /// The merged asset window across every constituent.
    ///
    /// Ordered by capture timestamp with the asset id as tiebreak and computed
    /// at render with nothing stored, so two viewers of the same group see the
    /// same order. Entries from an unreachable origin **still render from the
    /// local index** — nothing is removed for being unreachable, because
    /// unreachable is an outage and removal looks like data loss.
    public func assets(in groupID: AlbumGroupID, offset: Int, limit: Int) async throws -> Page<LibraryAsset> {
        let request = PageRequest(offset: offset, limit: limit)
        guard let album = group(groupID) else { return Page(items: [], request: request, totalCount: 0) }
        // A blocked origin drops out of *this viewer's* aggregate, without
        // affecting any other participant's view — blocking is per-origin and
        // local, not a group-level removal.
        let visible = album.constituents.filter { $0.availability.rendersFromLocalIndex }
        var merged: [LibraryAsset] = []
        for constituent in visible {
            let page = try await store.assets(
                matching: TimelineQuery(albumID: constituent.albumID),
                offset: 0,
                limit: min(constituent.assetCount, request.offset + request.limit)
            )
            merged.append(contentsOf: page.items)
        }
        let ordered = merged.sorted(by: LibraryAsset.isOrderedNewestFirst)
        return Page(
            items: MockQueryEngine.window(ordered, request: request),
            request: request,
            totalCount: nil
        )
    }

    /// Create a group and assert this user's own album into it.
    public func createGroup(name: String, constituent: AlbumID) async throws -> AlbumGroupID {
        let identifier = MockIdentifiers.albumGroupID(seed: configuration.seed, ordinal: 900 + groupList.count)
        let count = await store.albumCount(constituent)
        setGroup(AggregatedAlbum(
            id: identifier,
            groupName: Lww(current: Stamped(
                value: name,
                timestamp: configuration.clock.now,
                author: MockTagIdentity.authoringDevice(seed: configuration.seed)
            )),
            constituents: [
                AggregatedConstituent(
                    albumID: constituent,
                    homeServer: "capsule.example",
                    availability: .available,
                    assetCount: count
                ),
            ]
        ))
        await federationChanges.send(())
        return identifier
    }

    /// Assert one of this user's albums into an existing group.
    ///
    /// The only way to join: inclusion requires both membership and an
    /// assertion, so a stranger's album cannot inject itself into anyone's view.
    /// A member hint only says where to *ask*; membership does the admitting.
    public func joinGroup(_ groupID: AlbumGroupID, with constituent: AlbumID) async throws {
        guard var album = group(groupID) else {
            throw CapsuleError(code: .albumNotAvailable, detail: "CapsuleMock: unknown group")
        }
        guard await store.container(constituent) != nil else {
            throw CapsuleError(
                code: .uploadOwnerNotPermitted,
                detail: "CapsuleMock: only an album this user is a member of may be asserted"
            )
        }
        guard !album.constituents.contains(where: { $0.albumID == constituent }) else { return }
        album.constituents.append(AggregatedConstituent(
            albumID: constituent,
            homeServer: "capsule.example",
            availability: .available,
            assetCount: await store.albumCount(constituent)
        ))
        setGroup(album)
        await federationChanges.send(())
    }

    /// Leave by **removing your own assertion**.
    ///
    /// There is deliberately no group-level kick: each contributor is sovereign
    /// over their own constituent, so this can only ever remove yours. It drops
    /// out of every participant's aggregate on their next sync. Unsharing as
    /// well cuts read access to the historical photographs — a separate decision
    /// precisely because leaving and revoking are different intentions.
    public func leaveGroup(_ groupID: AlbumGroupID, alsoUnshare: Bool) async throws {
        guard var album = group(groupID) else { return }
        let own = album.constituents.filter { $0.homeServer == "capsule.example" }
        album.constituents.removeAll { $0.homeServer == "capsule.example" }
        if album.constituents.isEmpty {
            removeGroup(groupID)
        } else {
            setGroup(album)
        }
        if alsoUnshare {
            for constituent in own {
                try await store.removeMember(handle: "morgan@capsule.example", from: constituent.albumID)
            }
        }
        await federationChanges.send(())
    }

    /// Set this viewer's cover — a **per-viewer** preference, never shared
    /// state, so it cannot be used to change what anyone else sees.
    public func setCover(_ assetID: AssetID?, for groupID: AlbumGroupID) async throws {
        guard var album = group(groupID) else { return }
        album.coverAssetID = assetID
        setGroup(album)
        await federationChanges.send(())
    }

    public nonisolated func changes() -> AsyncStream<Void> {
        federationChanges.subscribe()
    }
}
