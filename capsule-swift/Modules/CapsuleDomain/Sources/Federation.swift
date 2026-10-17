import CapsuleFoundation
import Foundation

// MARK: - PeerState

/// How a federated peer is behaving right now (*Federation — Per-Peer
/// Containment*, *Robustness Against Connectivity Loss*).
///
/// Each peer is its own blast-radius boundary: a busy or hostile peer cannot
/// starve good ones, and a peer's state never removes local index entries. The
/// assets are unreachable, **not deleted** — which is the difference between a
/// temporary outage and data loss, and the UI must never conflate them.
public enum PeerState: Sendable, Equatable, Hashable {
    /// Pulls are succeeding.
    case reachable
    /// Pulls are failing, but within the downtime budget. Still shown as a
    /// transient outage.
    case degraded(consecutiveFailedDays: Int)
    /// The error budget tripped a circuit breaker; requests are short-circuited
    /// until the backoff elapses (5 / 30 / 60 minutes).
    case circuitOpen(until: CapsuleTimestamp)
    /// The peer is on this server's or this user's blocklist. A moderation
    /// decision, not a failure.
    case blocked
    /// Never contacted, or state not yet known.
    case unknown

    /// Whether a pull may be attempted.
    public var permitsPull: Bool {
        switch self {
        case .reachable, .degraded: true
        case .circuitOpen, .blocked, .unknown: false
        }
    }
}

// MARK: - Peer

/// A federated peer server.
public struct Peer: Sendable, Equatable, Identifiable, Hashable {
    public var id: PeerID
    /// The peer's canonical origin.
    public var origin: String
    public var state: PeerState
    public var firstSeen: CapsuleTimestamp
    /// The last pull that actually succeeded. The clock the downtime budget
    /// runs against.
    public var lastSuccessfulPullAt: CapsuleTimestamp?

    public init(
        id: PeerID,
        origin: String,
        state: PeerState,
        firstSeen: CapsuleTimestamp,
        lastSuccessfulPullAt: CapsuleTimestamp? = nil
    ) {
        self.id = id
        self.origin = origin
        self.state = state
        self.firstSeen = firstSeen
        self.lastSuccessfulPullAt = lastSuccessfulPullAt
    }
}

// MARK: - PeerCompartment

/// One peer's containment budgets — the per-peer compartmentalization layer
/// (*Federation — Per-Peer Containment*).
///
/// Transfer and storage are bounded **separately and per peer**: the transfer
/// budgets stop one peer monopolising throughput, and the caching budget stops a
/// user pulling from many peers exhausting home storage while staying inside
/// every individual peer's transfer budget.
public struct PeerCompartment: Sendable, Equatable, Identifiable, Hashable {
    public var peerID: PeerID
    /// Remaining events in the current hour, when the server reports it.
    public var eventsRemainingThisHour: UInt64?
    /// Remaining transfer bytes in the current hour.
    public var bytesRemainingThisHour: UInt64?
    /// Bytes cached on the receiving user's behalf from this peer. Charged to
    /// the **receiver's** quota, deduped.
    public var cachedBytes: UInt64
    /// The per-`(receiving user, source peer)` cache ceiling — 25% of the
    /// receiver's hard quota per source peer by default.
    public var cacheBudgetBytes: UInt64?
    /// How much malformed input the peer may still send before its circuit
    /// trips.
    public var errorBudgetRemaining: UInt32?

    public var id: PeerID { peerID }

    public init(
        peerID: PeerID,
        eventsRemainingThisHour: UInt64? = nil,
        bytesRemainingThisHour: UInt64? = nil,
        cachedBytes: UInt64 = 0,
        cacheBudgetBytes: UInt64? = nil,
        errorBudgetRemaining: UInt32? = nil
    ) {
        self.peerID = peerID
        self.eventsRemainingThisHour = eventsRemainingThisHour
        self.bytesRemainingThisHour = bytesRemainingThisHour
        self.cachedBytes = cachedBytes
        self.cacheBudgetBytes = cacheBudgetBytes
        self.errorBudgetRemaining = errorBudgetRemaining
    }

    /// Whether a further pull would cross this peer's caching budget.
    public var isCacheBudgetExhausted: Bool {
        guard let cacheBudgetBytes else { return false }
        return cachedBytes >= cacheBudgetBytes
    }
}

// MARK: - OriginAvailability

/// Whether one aggregated-album constituent's origin can be reached
/// (*Federation — Robustness Against Connectivity Loss*).
///
/// The escalation from ``temporarilyUnreachable`` to
/// ``ownedByUnreachableServer`` happens after the **downtime budget** — 30 days
/// of failed pulls by default. Neither state removes anything: local index
/// entries are **never** removed, because resuming federation when the server
/// recovers re-validates and re-enables the album.
public enum OriginAvailability: Sendable, Equatable, Hashable {
    /// Pulls from this origin are working.
    case available
    /// Currently unreachable, inside the downtime budget. Rendered from the
    /// local index with a transient badge.
    case temporarilyUnreachable(since: CapsuleTimestamp)
    /// Past the downtime budget. The album is marked degraded — "owned by an
    /// unreachable server" — and stays listed.
    case ownedByUnreachableServer(since: CapsuleTimestamp)
    /// The origin was blocked by a moderation decision, so its constituent
    /// drops out of this viewer's aggregate.
    case blocked

    /// Whether entries from this origin still render, from whatever the local
    /// index holds. True for everything except a blocked origin — unreachable
    /// is not deleted.
    public var rendersFromLocalIndex: Bool {
        self != .blocked
    }
}

// MARK: - AggregatedConstituent

/// One contributor's container album inside an aggregated album.
///
/// A constituent appears in the local aggregate **only if** the local user is a
/// member of that album *and* it asserts the group id. A stranger's album
/// cannot inject itself into anyone's view: without an invite its assertion is
/// not even decryptable. `member_hint` only says where to *ask*; membership does
/// the admitting.
public struct AggregatedConstituent: Sendable, Equatable, Identifiable, Hashable {
    /// The contributor's own container album.
    public var albumID: AlbumID
    /// The origin that homes it.
    public var homeServer: String
    /// The peer, when the origin is not this user's own home server.
    public var peerID: PeerID?
    /// Whether this origin can currently be reached.
    public var availability: OriginAvailability
    /// How many assets from this constituent are in the local index.
    public var assetCount: Int

    public var id: AlbumID { albumID }

    public init(
        albumID: AlbumID,
        homeServer: String,
        peerID: PeerID? = nil,
        availability: OriginAvailability,
        assetCount: Int
    ) {
        self.albumID = albumID
        self.homeServer = homeServer
        self.peerID = peerID
        self.availability = availability
        self.assetCount = assetCount
    }
}

// MARK: - AggregatedAlbum

/// An aggregated federated album — N ordinary container albums, one per
/// contributor, presented as one logical album
/// (*Federation — Federated Shared Albums*).
///
/// It is a **view** in exactly the sense ``ViewAlbum`` is: computed at render,
/// holding no keys, owning no assets, and no access-control boundary. There is
/// **zero new server surface** — servers never learn that a group exists — and
/// there is deliberately no shared mutable group object, because that would be
/// precisely the cross-server multi-writer state the design defers.
///
/// Two honest limitations the UI must not paper over:
///
/// - **There is no group-level kick.** Each contributor is sovereign over their
///   own constituent. You can stop someone seeing *your* photos by unsharing;
///   nobody can remove someone else's constituent from other viewers' aggregate.
/// - **Partial views degrade visibly.** An unreachable origin renders from the
///   local index with a per-origin badge; nothing is removed.
public struct AggregatedAlbum: Sendable, Equatable, Identifiable, Hashable {
    public var id: AlbumGroupID
    /// The group name, converging by LWW across every participant's assertion.
    public var groupName: Lww<String>
    /// The constituents this viewer can actually see.
    public var constituents: [AggregatedConstituent]
    /// The cover — a **per-viewer** preference, never shared state. `nil` falls
    /// back to the newest constituent asset.
    public var coverAssetID: AssetID?

    public init(
        id: AlbumGroupID,
        groupName: Lww<String>,
        constituents: [AggregatedConstituent],
        coverAssetID: AssetID? = nil
    ) {
        self.id = id
        self.groupName = groupName
        self.constituents = constituents
        self.coverAssetID = coverAssetID
    }

    /// Total assets across every constituent in the local index.
    public var assetCount: Int {
        constituents.reduce(0) { $0 + $1.assetCount }
    }

    /// The origins whose content is currently incomplete — what the
    /// "photos from *X* currently unavailable" surface enumerates.
    public var unavailableOrigins: [AggregatedConstituent] {
        constituents.filter { $0.availability != .available }
    }

    /// Whether every origin is reachable.
    public var isFullyAvailable: Bool {
        unavailableOrigins.isEmpty
    }
}
