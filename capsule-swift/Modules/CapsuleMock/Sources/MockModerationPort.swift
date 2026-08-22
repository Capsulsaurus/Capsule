import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

// MARK: - ModerationPort

extension MockFederationStore: ModerationPort {
    public func blocks() async throws -> [BlockEntry] {
        blocks
    }

    /// Block a user or a peer server.
    ///
    /// **Per-origin**: it drops that origin's constituent from this viewer's
    /// aggregated albums and stops pulls from it, without affecting any other
    /// participant's view. There is no global takedown here, because no
    /// participant has authority over another's view.
    public func block(_ subject: BlockEntry.Subject) async throws {
        let identifier = Self.subjectKey(subject)
        guard !blocks.contains(where: { $0.id == identifier }) else { return }
        let entry = BlockEntry(id: identifier, subject: subject, blockedAt: configuration.clock.now)
        setBlocks(blocks + [entry])
        await applyBlocks()
    }

    public func unblock(_ identifier: String) async throws {
        setBlocks(blocks.filter { $0.id != identifier })
        await federationChanges.send(())
    }

    public func report(
        _ subject: ModerationReport.Subject,
        reason: ModerationReport.Reason
    ) async throws -> ModerationReport {
        try reason.requireWritable()
        return try fileReport(subject: subject, reason: reason)
    }

    /// Federated peers with their containment state — what a "why is this album
    /// unavailable" surface reads.
    ///
    /// A peer's state **never removes local index entries**. The assets are
    /// unreachable, not deleted, and the difference between a temporary outage
    /// and data loss is the whole reason this is a state rather than a
    /// deletion.
    public func peers() async throws -> [Peer] {
        let clock = configuration.clock
        let degraded = configuration.federationIsDegraded
        let blocked = Set(blocks.compactMap { entry -> String? in
            if case let .peer(peerID) = entry.subject { return peerID.rawValue }
            return nil
        })
        return ["photos.other.example", "legacy.example", "blocked.example"].map { origin in
            Peer(
                id: MockIdentifiers.peerID(origin: origin),
                origin: origin,
                state: Self.state(
                    origin: origin,
                    degraded: degraded,
                    isBlocked: blocked.contains(origin) || origin == "blocked.example",
                    clock: clock
                ),
                firstSeen: clock.offset(days: -365),
                lastSuccessfulPullAt: origin == "legacy.example" ? clock.offset(days: -44) : clock.offset(days: -1)
            )
        }
    }

    /// One peer's containment budgets.
    ///
    /// Transfer and storage are bounded **separately and per peer**: transfer
    /// budgets stop one peer monopolising throughput, and the caching budget
    /// stops a user pulling from many peers exhausting home storage while
    /// staying inside every individual peer's transfer budget.
    public func compartment(for peerID: PeerID) async throws -> PeerCompartment? {
        guard try await peers().contains(where: { $0.id == peerID }) else { return nil }
        let hash = MockHash.value(seed: configuration.seed, index: peerID.rawValue.utf8.count, salt: .byteSize)
        let budget = UInt64(128 * 1_073_741_824)
        return PeerCompartment(
            peerID: peerID,
            eventsRemainingThisHour: UInt64(MockHash.integer(hash, in: 0 ... 4000)),
            bytesRemainingThisHour: UInt64(MockHash.integer(MockHash.mix(hash), in: 0 ... 8_000_000_000)),
            cachedBytes: UInt64(MockHash.integer(MockHash.mix(hash &+ 3), in: 0 ... Int(budget))),
            cacheBudgetBytes: budget,
            errorBudgetRemaining: UInt32(MockHash.integer(MockHash.mix(hash &+ 5), in: 0 ... 64))
        )
    }

    // MARK: Helpers

    private static func state(
        origin: String,
        degraded: Bool,
        isBlocked: Bool,
        clock: MockClock
    ) -> PeerState {
        if isBlocked { return .blocked }
        guard degraded else { return .reachable }
        switch origin {
        case "legacy.example": return .degraded(consecutiveFailedDays: 44)
        case "photos.other.example": return .circuitOpen(until: clock.offset(seconds: 1800))
        default: return .reachable
        }
    }

    private static func subjectKey(_ subject: BlockEntry.Subject) -> String {
        switch subject {
        case let .user(handle): handle
        case let .peer(peerID): peerID.rawValue
        }
    }

    /// Drop a blocked origin's constituent from this viewer's aggregates.
    private func applyBlocks() async {
        let blockedOrigins = Set(blocks.map { entry -> String in
            switch entry.subject {
            case let .user(handle): String(handle.split(separator: "@").last ?? "")
            case let .peer(peerID): peerID.rawValue
            }
        })
        for var album in groupList {
            album.constituents = album.constituents.map { constituent in
                guard blockedOrigins.contains(constituent.homeServer) else { return constituent }
                var updated = constituent
                updated.availability = .blocked
                return updated
            }
            setGroup(album)
        }
        await federationChanges.send(())
    }
}
