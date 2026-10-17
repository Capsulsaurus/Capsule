import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - FederationPort

/// Aggregated albums across home servers.
///
/// An aggregated album is a **view**: N ordinary container albums, one per
/// contributor, presented as one. There is zero new server surface — servers
/// never learn a group exists — and no shared mutable group object, so
/// everything here operates on *this user's own* constituent and on what they
/// can already decrypt.
public protocol FederationPort: Sendable {
    /// Every aggregated album this viewer can see.
    ///
    /// Maps to `federation.list_aggregated_albums`.
    func aggregatedAlbums() async throws -> [AggregatedAlbum]

    /// One aggregated album.
    ///
    /// Maps to `federation.get_aggregated_album`.
    func aggregatedAlbum(_ id: AlbumGroupID) async throws -> AggregatedAlbum?

    /// The merged asset window across every constituent.
    ///
    /// Ordered by capture timestamp with the asset id as tiebreak, computed at
    /// render with nothing stored — so two viewers of the same group see the
    /// same order. Entries from an unreachable origin still render from the
    /// local index; **nothing is removed** for being unreachable.
    ///
    /// Maps to `federation.aggregated_assets`.
    func assets(in groupID: AlbumGroupID, offset: Int, limit: Int) async throws -> Page<LibraryAsset>

    /// Create a group and assert this user's own album into it.
    ///
    /// Maps to `federation.create_group`.
    func createGroup(name: String, constituent: AlbumID) async throws -> AlbumGroupID

    /// Assert one of this user's albums into an existing group — the only way to
    /// join, since inclusion requires both membership and an assertion.
    ///
    /// Maps to `federation.assert_membership`.
    func joinGroup(_ groupID: AlbumGroupID, with constituent: AlbumID) async throws

    /// Leave by **removing your own assertion**. Your constituent drops out of
    /// every participant's aggregate on their next sync.
    ///
    /// There is deliberately **no group-level kick**: each contributor is
    /// sovereign over their own constituent, so this can only ever remove your
    /// own. Optionally unshare your container as well, to cut read access to the
    /// historical photos.
    ///
    /// Maps to `federation.retract_assertion`.
    func leaveGroup(_ groupID: AlbumGroupID, alsoUnshare: Bool) async throws

    /// Set this viewer's cover for a group — a **per-viewer** preference, never
    /// shared state.
    ///
    /// Maps to `settings.set_aggregated_cover`.
    func setCover(_ assetID: AssetID?, for groupID: AlbumGroupID) async throws

    /// A stream that fires when a group's membership or availability changes.
    func changes() -> AsyncStream<Void>
}

// MARK: - PeeringPort

/// LAN transfers between this user's own devices.
///
/// Not federation: peering moves originals between one account's devices without
/// touching the WAN, and is the designated degraded-mode path when the WAN is
/// persistently adverse. A blob from a peer is verified against its content
/// address exactly as a server blob is — a peer is a device on a network, not an
/// authority.
public protocol PeeringPort: Sendable {
    /// Whether peering is enabled on this device.
    ///
    /// Maps to `peering.is_enabled`.
    func isEnabled() async -> Bool

    /// Enable or disable peering.
    ///
    /// Maps to `peering.set_enabled`.
    func setEnabled(_ enabled: Bool) async throws

    /// Devices discovered on the local network, with their trust state.
    ///
    /// Discovery alone proves nothing: only a device already in the account's
    /// directory can reach ``LocalPeer/Trust/paired``.
    ///
    /// Maps to `peering.discovered_peers`.
    func discoveredPeers() async throws -> [LocalPeer]

    /// Transfers in flight in either direction.
    ///
    /// Maps to `peering.active_transfers`.
    func activeTransfers() async throws -> [PeeringTransfer]

    /// Request originals from a peer that holds them.
    ///
    /// Maps to `peering.request_originals`.
    func requestOriginals(for assetIDs: [AssetID], from peer: DeviceID) async throws

    /// A stream of peer and transfer updates.
    func changes() -> AsyncStream<[LocalPeer]>
}

// MARK: - ModerationPort

/// Blocking and reporting.
public protocol ModerationPort: Sendable {
    /// This user's blocklist.
    ///
    /// Maps to `moderation.list_blocks`.
    func blocks() async throws -> [BlockEntry]

    /// Block a user or a peer server.
    ///
    /// Blocking is **per-origin**: it drops that origin's constituent from this
    /// viewer's aggregated albums and stops pulls from it, without affecting any
    /// other participant's view.
    ///
    /// Maps to `moderation.block`.
    func block(_ subject: BlockEntry.Subject) async throws

    /// Unblock.
    ///
    /// Maps to `moderation.unblock`.
    func unblock(_ id: String) async throws

    /// File a report.
    ///
    /// Rate-limited per reporter and per subject — backpressure is what defeats
    /// mass-report abuse rather than amplifying it.
    ///
    /// Maps to `moderation.report`.
    func report(
        _ subject: ModerationReport.Subject,
        reason: ModerationReport.Reason
    ) async throws -> ModerationReport

    /// Federated peers this device knows about, with their containment state —
    /// what a "why is this album unavailable" surface reads.
    ///
    /// Maps to `federation.list_peers`.
    func peers() async throws -> [Peer]

    /// One peer's containment budgets.
    ///
    /// Maps to `federation.peer_compartment`.
    func compartment(for peerID: PeerID) async throws -> PeerCompartment?
}
