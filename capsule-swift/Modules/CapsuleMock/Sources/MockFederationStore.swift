import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - MockFederationStore

/// Aggregated albums across home servers, LAN peering between this user's own
/// devices, and the moderation state that gates both.
///
/// Together because a block is the thing that removes a constituent from an
/// aggregate, and a peer's containment state is what a "why is this album
/// unavailable" surface reads. Splitting them would let a blocked origin keep
/// contributing photographs to a view.
public actor MockFederationStore {
    nonisolated let store: MockLibraryStore
    nonisolated let configuration: MockConfiguration

    private var groups: [AlbumGroupID: AggregatedAlbum]
    private var groupOrder: [AlbumGroupID]
    private var peeringEnabled = true
    private var blockEntries: [BlockEntry] = []
    private var reports: [ModerationReport] = []
    private var reportCount = 0

    nonisolated let federationChanges = ChangeBroadcaster<Void>()
    nonisolated let peeringChanges = ChangeBroadcaster<[LocalPeer]>()

    public init(store: MockLibraryStore, configuration: MockConfiguration) {
        self.store = store
        self.configuration = configuration
        let seeded = MockFederationSeed.groups(configuration: configuration)
        groupOrder = seeded.map(\.id)
        groups = Dictionary(uniqueKeysWithValues: seeded.map { ($0.id, $0) })
    }

    // MARK: State

    var groupList: [AggregatedAlbum] { groupOrder.compactMap { groups[$0] } }
    var isPeeringEnabled: Bool { peeringEnabled }
    var blocks: [BlockEntry] { blockEntries }
    var filedReports: [ModerationReport] { reports }

    func group(_ identifier: AlbumGroupID) -> AggregatedAlbum? { groups[identifier] }

    func setGroup(_ album: AggregatedAlbum) {
        if groups[album.id] == nil { groupOrder.append(album.id) }
        groups[album.id] = album
    }

    func removeGroup(_ identifier: AlbumGroupID) {
        groups[identifier] = nil
        groupOrder.removeAll { $0 == identifier }
    }

    func setPeeringEnabled(_ enabled: Bool) { peeringEnabled = enabled }
    func setBlocks(_ value: [BlockEntry]) { blockEntries = value }

    /// File a report and return it.
    ///
    /// Rate-limited per reporter and per subject: backpressure is what defeats
    /// mass-report abuse rather than amplifying it, so exceeding the window is a
    /// refusal rather than a slower accept.
    func fileReport(subject: ModerationReport.Subject, reason: ModerationReport.Reason) throws -> ModerationReport {
        guard reportCount < Self.reportsPerWindow else {
            throw CapsuleError(
                code: .moderationReportRateLimited,
                detail: "CapsuleMock: report rate limit reached"
            )
        }
        reportCount += 1
        let report = ModerationReport(
            id: MockHash.hex(
                MockHash.value(seed: configuration.seed, index: reportCount, salt: .identity, sub: 31),
                digits: 16
            ),
            subject: subject,
            reason: reason,
            submittedAt: configuration.clock.now
        )
        reports.append(report)
        return report
    }

    static let reportsPerWindow = 5
}

// MARK: - MockFederationSeed

enum MockFederationSeed {
    /// Two aggregated albums: one fully available, one degraded.
    ///
    /// An aggregated album is a **view** — N ordinary container albums, one per
    /// contributor, presented as one. Servers never learn a group exists, so
    /// there is no shared mutable group object and no group-level kick: each
    /// contributor is sovereign over their own constituent. The degraded one
    /// exists because an unreachable origin still renders from the local index,
    /// and a surface that has only ever shown the happy case will show an empty
    /// album instead of a badge.
    static func groups(configuration: MockConfiguration) -> [AggregatedAlbum] {
        let seed = configuration.seed
        let clock = configuration.clock
        let degraded = configuration.federationIsDegraded
        return [
            AggregatedAlbum(
                id: MockIdentifiers.albumGroupID(seed: seed, ordinal: 0),
                groupName: name("summer-share", clock: clock, seed: seed),
                constituents: [
                    constituent(seed: seed, ordinal: 1, origin: "capsule.example", availability: .available, count: 214),
                    constituent(
                        seed: seed,
                        ordinal: 2,
                        origin: "photos.other.example",
                        availability: degraded
                            ? .temporarilyUnreachable(since: clock.offset(days: -4))
                            : .available,
                        count: 168
                    ),
                ]
            ),
            AggregatedAlbum(
                id: MockIdentifiers.albumGroupID(seed: seed, ordinal: 1),
                groupName: name("family-archive", clock: clock, seed: seed),
                constituents: [
                    constituent(seed: seed, ordinal: 3, origin: "capsule.example", availability: .available, count: 92),
                    constituent(
                        seed: seed,
                        ordinal: 4,
                        origin: "legacy.example",
                        availability: degraded
                            ? .ownedByUnreachableServer(since: clock.offset(days: -44))
                            : .available,
                        count: 311
                    ),
                    constituent(seed: seed, ordinal: 5, origin: "blocked.example", availability: .blocked, count: 0),
                ]
            ),
        ]
    }

    private static func name(_ text: String, clock: MockClock, seed: UInt64) -> Lww<String> {
        Lww(current: Stamped(
            value: text,
            timestamp: clock.offset(days: -120),
            author: MockTagIdentity.authoringDevice(seed: seed)
        ))
    }

    private static func constituent(
        seed: UInt64,
        ordinal: Int,
        origin: String,
        availability: OriginAvailability,
        count: Int
    ) -> AggregatedConstituent {
        AggregatedConstituent(
            albumID: MockIdentifiers.albumID(seed: seed, ordinal: ordinal),
            homeServer: origin,
            peerID: origin == "capsule.example" ? nil : MockIdentifiers.peerID(origin: origin),
            availability: availability,
            assetCount: count
        )
    }
}
