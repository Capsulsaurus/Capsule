import CapsuleDomain
import CapsulePorts
import Foundation
import Observation

// MARK: - PeerAvailability

/// A peer's state, resolved against the **downtime budget** (*Federation —
/// Robustness Against Connectivity Loss*).
///
/// ``PeerState/degraded(consecutiveFailedDays:)`` is one wire state covering two
/// very different user-facing situations, and the budget is what separates them:
/// inside it the album is having a bad day, past it the album is "owned by an
/// unreachable server". Neither removes anything.
public enum PeerAvailability: Sendable, Equatable, Hashable {
    case reachable
    /// Failing, but inside the budget. A transient outage.
    case transientOutage(consecutiveFailedDays: Int)
    /// The circuit breaker is open; requests are short-circuited until the
    /// backoff elapses (5 / 30 / 60 minutes).
    case backingOff(until: CapsuleTimestamp)
    /// Past the budget. Rendered as owned by an unreachable server — still
    /// listed, still counted, never presented as deleted.
    case unreachableServer(consecutiveFailedDays: Int)
    /// A moderation decision, not a failure.
    case blocked
    case unknown

    /// Whether this peer is one the "why is this album incomplete" surface
    /// should name.
    public var isImpaired: Bool {
        self != .reachable && self != .unknown
    }
}

// MARK: - FederationViewModel

/// Peer servers, their compartments, and the aggregated albums built across
/// them (*Federation*).
///
/// The invariant every accessor here protects: **local index entries are never
/// removed**. An unreachable origin's photographs still count, still list, and
/// still come back on their own when the server recovers — so this model reports
/// *availability* alongside the entries rather than filtering by it. Filtering
/// would render an outage as data loss, which is the one thing the design says
/// the UI must never do.
@MainActor
@Observable
public final class FederationViewModel {
    public private(set) var albums: [AggregatedAlbum] = []
    public private(set) var peers: [Peer] = []
    public private(set) var compartments: [PeerID: PeerCompartment] = [:]
    public private(set) var phase: SharingPhase = .loading
    public private(set) var connection: ConnectionClass?
    public var selection: AlbumGroupID?

    private let federation: any FederationPort
    private let moderation: any ModerationPort
    private let connectivity: SharingConnectivity
    // Not observed and not isolated: it is a cancellation handle, never
    // rendered, and `deinit` must be able to cancel it without hopping actors.
    @ObservationIgnored
    private nonisolated(unsafe) var observation: Task<Void, Never>?

    /// The default downtime budget: 30 days of failed pulls before an album is
    /// marked as owned by an unreachable server.
    public static let downtimeBudgetDays = 30

    public init(
        federation: any FederationPort,
        moderation: any ModerationPort,
        connectivity: SharingConnectivity = SharingConnectivity()
    ) {
        self.federation = federation
        self.moderation = moderation
        self.connectivity = connectivity
    }

    deinit {
        observation?.cancel()
    }

    // MARK: Derived state

    public var selectedAlbum: AggregatedAlbum? {
        guard let selection else { return nil }
        return albums.first { $0.id == selection }
    }

    /// Origins whose content is currently incomplete, for one album.
    ///
    /// Reported, not removed: the constituents stay in ``AggregatedAlbum``, and
    /// their assets stay in the counts below.
    public func unavailableOrigins(in album: AggregatedAlbum) -> [AggregatedConstituent] {
        album.unavailableOrigins
    }

    /// How many entries an album still renders from the local index.
    ///
    /// Everything except a blocked origin, whose constituent genuinely drops out
    /// of *this viewer's* aggregate — a moderation decision the user made, not a
    /// failure. An unreachable origin contributes its full count.
    public func renderedAssetCount(in album: AggregatedAlbum) -> Int {
        album.constituents
            .filter(\.availability.rendersFromLocalIndex)
            .reduce(0) { $0 + $1.assetCount }
    }

    /// Whether an album is degraded — some origin cannot be reached — while
    /// still rendering everything the local index holds.
    public func isDegraded(_ album: AggregatedAlbum) -> Bool {
        !album.isFullyAvailable
    }

    /// Whether any constituent has passed the downtime budget.
    public func hasUnreachableOwner(_ album: AggregatedAlbum) -> Bool {
        album.constituents.contains {
            if case .ownedByUnreachableServer = $0.availability { return true }
            return false
        }
    }

    /// Resolve a peer against the downtime budget.
    public func availability(of peer: Peer) -> PeerAvailability {
        switch peer.state {
        case .reachable:
            .reachable
        case let .degraded(days):
            if days >= Self.downtimeBudgetDays {
                .unreachableServer(consecutiveFailedDays: days)
            } else {
                .transientOutage(consecutiveFailedDays: days)
            }
        case let .circuitOpen(until):
            .backingOff(until: until)
        case .blocked:
            .blocked
        case .unknown:
            .unknown
        }
    }

    /// Peers past the downtime budget. Present in ``peers`` as well — they are
    /// reported as unreachable, never dropped from the list.
    public var unreachablePeers: [Peer] {
        peers.filter {
            if case .unreachableServer = availability(of: $0) { return true }
            return false
        }
    }

    // MARK: Actions

    public func load() async {
        await reload()
        observeChanges()
    }

    public func reload() async {
        connection = await connectivity.probe()
        do {
            albums = try await federation.aggregatedAlbums()
            peers = try await moderation.peers()
            compartments = await loadCompartments(for: peers)
            selection = selection ?? albums.first?.id
            phase = albums.isEmpty && peers.isEmpty ? .empty : .ready
        } catch {
            phase = SharingPhase.resolve(error, connection: connection)
        }
    }

    /// Leave a group by retracting **your own** assertion.
    ///
    /// The only removal this screen can perform. There is deliberately no
    /// group-level kick: each contributor is sovereign over their own
    /// constituent, so nobody can remove someone else's from other viewers'
    /// aggregates.
    public func leave(_ groupID: AlbumGroupID, alsoUnshare: Bool) async {
        do {
            try await federation.leaveGroup(groupID, alsoUnshare: alsoUnshare)
            await reload()
        } catch {
            phase = SharingPhase.resolve(error, connection: connection)
        }
    }

    /// Compartments are best-effort: a peer that will not answer for its own
    /// budgets is exactly the peer whose album is degraded, so a failure here
    /// must not take the screen down with it.
    private func loadCompartments(for peers: [Peer]) async -> [PeerID: PeerCompartment] {
        var resolved: [PeerID: PeerCompartment] = [:]
        for peer in peers {
            if let compartment = try? await moderation.compartment(for: peer.id) {
                resolved[peer.id] = compartment
            }
        }
        return resolved
    }

    private func observeChanges() {
        observation?.cancel()
        let port = federation
        observation = Task { [weak self] in
            for await _ in port.changes() {
                guard !Task.isCancelled else { return }
                await self?.reload()
            }
        }
    }
}
