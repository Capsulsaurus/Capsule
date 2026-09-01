import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - MockLadderState

/// What this device holds for an asset, and where the asset stands with the
/// server. The two are derived together because they constrain each other: an
/// asset cannot be `durable` while its original is still on the phone that took
/// it, and it cannot report `fullResolutionUnavailable` while holding the
/// original.
public struct MockLadderState: Sendable, Equatable {
    public var representations: LocalRepresentations
    public var state: AssetSyncState
}

public extension MockLibrary {
    /// The degrade ladder and the sync state for one asset.
    ///
    /// The states are mutually exclusive and ordered by how much they override:
    /// a schema this build cannot read makes every other question moot, a
    /// quarantined asset is held whatever its bytes say, and only then do the
    /// ordinary transfer states apply. Getting that order wrong is how an asset
    /// ends up rendering as "uploading" when it is really being withheld for a
    /// human decision.
    func representationState(for ref: MockAssetRef) -> MockLadderState {
        let held = heldTiers(for: ref)
        if let exceptional = exceptionalState(for: ref, held: held) {
            return exceptional
        }
        return ordinaryState(for: ref, held: held)
    }

    /// The tiers this device holds, before any scenario degradation.
    ///
    /// Everything holds the LQIP — it rides inside the metadata blob, so it is
    /// present the instant metadata syncs, which is precisely why a missing
    /// thumbnail is never a blank tile.
    private func heldTiers(for ref: MockAssetRef) -> Set<RepresentationTier> {
        let hash = MockHash.value(seed: profile.seed, index: ref.derivationIndex, salt: .representation)
        var tiers: Set<RepresentationTier> = [.dominantColour, .lqip]
        if !MockHash.occurs(hash, perMille: 70) { tiers.insert(.thumbnail) }
        if MockHash.occurs(MockHash.mix(hash), perMille: 560) { tiers.insert(.preview) }
        if MockHash.occurs(MockHash.mix(hash &+ 7), perMille: 230) { tiers.insert(.original) }
        return tiers
    }

    /// The states that override everything else, in priority order.
    private func exceptionalState(
        for ref: MockAssetRef,
        held: Set<RepresentationTier>
    ) -> MockLadderState? {
        let derivation = ref.derivationIndex
        if isFromNewerVersion(derivationIndex: derivation) {
            return MockLadderState(
                representations: LocalRepresentations(heldTiers: held.subtracting([.original])),
                state: .writtenByNewerVersion(
                    SchemaAhead(surface: .sidecarSchema, found: "2", maxKnown: "1")
                )
            )
        }
        if let quarantine = quarantineState(derivationIndex: derivation) {
            return MockLadderState(representations: LocalRepresentations(heldTiers: held), state: quarantine)
        }
        if let unreadable = unreadableState(for: ref) {
            return MockLadderState(representations: LocalRepresentations(heldTiers: held), state: unreadable)
        }
        return nil
    }

    private func quarantineState(derivationIndex: Int) -> AssetSyncState? {
        guard profile.quarantinedPerMille > 0 else { return nil }
        let hash = MockHash.value(seed: profile.seed, index: derivationIndex, salt: .syncState, sub: 41)
        guard MockHash.occurs(hash, perMille: profile.quarantinedPerMille) else { return nil }
        return .quarantined(MockIdentifiers.quarantineID(seed: profile.seed, ordinal: derivationIndex))
    }

    /// Valid, but this device cannot open it — which is materially different
    /// from the asset being invalid, and is why ``UnreadableReason`` is its own
    /// enum rather than a ``RejectReason``.
    private func unreadableState(for ref: MockAssetRef) -> AssetSyncState? {
        guard profile.unreadablePerMille > 0 else { return nil }
        let hash = MockHash.value(seed: profile.seed, index: ref.derivationIndex, salt: .syncState, sub: 23)
        guard MockHash.occurs(hash, perMille: profile.unreadablePerMille) else { return nil }
        let reasons: [UnreadableReason] = [
            .noCodecForContentType(contentType(for: ref)),
            .localBytesCorrupt,
            .albumKeyNotDelivered,
            .albumUpgradePending,
        ]
        let reason = MockHash.element(MockHash.mix(hash), from: reasons) ?? .localBytesCorrupt
        return .unreadableOnThisDevice(reason)
    }

    /// Transfer states, plus the offline degradation.
    private func ordinaryState(for ref: MockAssetRef, held: Set<RepresentationTier>) -> MockLadderState {
        let derivation = ref.derivationIndex
        let hash = MockHash.value(seed: profile.seed, index: derivation, salt: .syncState)

        if MockHash.occurs(hash, perMille: profile.awaitingOriginalPerMille) {
            let holder = MockIdentifiers.deviceID(seed: profile.seed, ordinal: 1)
            return MockLadderState(
                representations: LocalRepresentations(heldTiers: held.subtracting([.original])),
                state: .awaitingOriginal(heldBy: holder)
            )
        }
        if MockHash.occurs(MockHash.mix(hash), perMille: profile.awaitingOriginalPerMille / 3) {
            return uploadingState(ref: ref, held: held)
        }
        guard profile.degradesRemoteRepresentations else {
            return MockLadderState(representations: LocalRepresentations(heldTiers: held), state: .durable)
        }
        return degradedState(derivationIndex: derivation, held: held)
    }

    private func uploadingState(ref: MockAssetRef, held: Set<RepresentationTier>) -> MockLadderState {
        let total = byteSize(for: ref, contentType: contentType(for: ref))
        let hash = MockHash.value(seed: profile.seed, index: ref.derivationIndex, salt: .syncState, sub: 5)
        let transferred = UInt64(MockHash.fraction(hash) * Double(total))
        return MockLadderState(
            representations: LocalRepresentations(heldTiers: held),
            state: .uploading(tier: .original, transferred: transferred, total: max(1, total))
        )
    }

    /// Offline degradation: a rung that would need a fetch is not available, so
    /// the asset renders at the best representation in hand.
    ///
    /// **Non-destructive.** Metadata and the index entry stay, the asset stays
    /// in every view it was in, and it re-fetches automatically once the
    /// connection comes back. That is the difference between a degraded library
    /// and a broken one, and it is the whole reason the offline scenario exists.
    private func degradedState(derivationIndex: Int, held: Set<RepresentationTier>) -> MockLadderState {
        let hash = MockHash.value(seed: profile.seed, index: derivationIndex, salt: .representation, sub: 9)
        var remaining = held.subtracting([.original])
        if MockHash.occurs(hash, perMille: 600) { remaining.remove(.preview) }
        if MockHash.occurs(MockHash.mix(hash), perMille: 120) { remaining.remove(.thumbnail) }
        let ladder = LocalRepresentations(heldTiers: remaining)
        guard !held.contains(.original) else {
            return MockLadderState(representations: LocalRepresentations(heldTiers: held), state: .durable)
        }
        return MockLadderState(
            representations: ladder,
            state: .fullResolutionUnavailable(bestAvailable: ladder.best)
        )
    }
}
