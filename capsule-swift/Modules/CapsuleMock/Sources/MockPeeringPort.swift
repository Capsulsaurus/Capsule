import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

// MARK: - PeeringPort

/// LAN transfers between this user's own devices.
///
/// Not federation: peering moves originals between one account's devices without
/// touching the WAN, and is the designated degraded-mode path when the WAN is
/// persistently adverse. A blob from a peer is verified against its content
/// address exactly as a server blob is — a peer is a device on a network, not an
/// authority.
extension MockFederationStore: PeeringPort {
    public func isEnabled() async -> Bool {
        isPeeringEnabled
    }

    public func setEnabled(_ enabled: Bool) async throws {
        setPeeringEnabled(enabled)
        await peeringChanges.send(try await discoveredPeers())
    }

    /// Devices discovered on the local network, with their trust state.
    ///
    /// Discovery alone proves nothing: only a device already in the account's
    /// directory reaches ``LocalPeer/Trust/paired``, and one seeded peer is
    /// merely `discovered` so the "we can see it but will not talk to it" state
    /// is on screen.
    public func discoveredPeers() async throws -> [LocalPeer] {
        guard isPeeringEnabled else { return [] }
        let seed = configuration.seed
        let clock = configuration.clock
        return [
            LocalPeer(
                id: MockIdentifiers.deviceID(seed: seed, ordinal: 1),
                model: "iPhone17,2",
                platform: .ios,
                trust: .paired,
                lastSeenAt: clock.offset(seconds: -40)
            ),
            LocalPeer(
                id: MockIdentifiers.deviceID(seed: seed, ordinal: 2),
                model: "Pixel 9 Pro",
                platform: .android,
                trust: .revoked,
                lastSeenAt: clock.offset(seconds: -900)
            ),
            LocalPeer(
                id: MockIdentifiers.deviceID(seed: seed, ordinal: 9),
                model: "unknown",
                platform: .linux,
                trust: .discovered,
                lastSeenAt: clock.offset(seconds: -12)
            ),
        ]
    }

    public func activeTransfers() async throws -> [PeeringTransfer] {
        guard isPeeringEnabled, store.library.assetCount > 0 else { return [] }
        let seed = configuration.seed
        return (0 ..< 2).compactMap { ordinal -> PeeringTransfer? in
            let index = 3 + ordinal * 11
            guard index < store.library.assetCount else { return nil }
            let ref = MockAssetRef(kind: .live, index: index)
            let total = store.library.byteSize(for: ref, contentType: store.library.contentType(for: ref))
            return PeeringTransfer(
                id: MockIdentifiers.blobHash(seed: seed, ordinal: index),
                peerID: MockIdentifiers.deviceID(seed: seed, ordinal: 1),
                direction: ordinal == 0 ? .receiving : .sending,
                assetID: ref.uuidString(seed: seed),
                blobRole: .original,
                transferredBytes: total / (2 + UInt64(ordinal)),
                totalBytes: max(1, total)
            )
        }
    }

    /// Request originals from a peer that holds them.
    ///
    /// Refused for a peer that is not paired. Network presence is not trust, and
    /// a request that succeeded against a merely-discovered device would be the
    /// whole point of the directory check thrown away.
    public func requestOriginals(for assetIDs: [AssetID], from peer: DeviceID) async throws {
        let peers = try await discoveredPeers()
        guard let match = peers.first(where: { $0.id == peer }), match.permitsTransfer else {
            throw CapsuleError(
                code: .uploadDeviceNotAuthorized,
                detail: "CapsuleMock: only a paired device in the account directory may serve originals"
            )
        }
        for assetID in assetIDs {
            guard let asset = await store.engine.asset(for: assetID) else { continue }
            await store.applyFetchOutcome(
                assetID,
                representations: asset.representations.adding(.original),
                state: .durable
            )
        }
        await peeringChanges.send(peers)
    }

    public nonisolated func changes() -> AsyncStream<[LocalPeer]> {
        peeringChanges.subscribe()
    }
}
