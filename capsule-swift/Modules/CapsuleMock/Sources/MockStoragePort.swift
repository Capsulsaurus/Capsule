import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

// MARK: - StoragePort

/// Local disk accounting and the verify-before-destroy gate.
///
/// Separate from quota because they answer different questions: quota is what
/// the *server* is charging, storage is what this *device* holds and whether it
/// is safe to stop holding it. Conflating them is how a cache-clearing feature
/// ends up deleting an only copy.
extension MockTransferStore: StoragePort {
    public func localBreakdown() async throws -> LocalStorageBreakdown {
        let library = store.library
        let reclaimed = reclaimedBytes
        let engine = await store.engine
        return breakdown {
            Self.makeBreakdown(library: library, engine: engine, reclaimed: reclaimed)
        }
    }

    /// Ask whether these assets are stored, indexed, and retrievable **right
    /// now**.
    ///
    /// A point-in-time fact, not a standing guarantee — see
    /// ``StorageVerification/authorisesRelease(at:)`` for the freshness rule the
    /// caller must apply before acting on it. A hash the client lists that the
    /// server does not associate with the asset comes back not-stored and
    /// not-indexed, **surfaced rather than omitted**, so a missing blob cannot
    /// be mistaken for one nobody asked about.
    public func verify(assetIDs: [AssetID], deep: Bool) async throws -> [StorageVerification] {
        try await behaviourGate.admit()
        let engine = await store.engine
        return assetIDs.map { assetID in
            let asset = engine.asset(for: assetID)
            let isDurable = asset?.syncState == .durable
            return StorageVerification(
                assetID: MockAssetRef.decode(assetID)?.uuidString(seed: configuration.seed) ?? "",
                durable: isDurable,
                blobs: Self.verdicts(assetID: assetID, seed: configuration.seed, isDurable: isDurable, deep: deep),
                checkedAt: configuration.clock.now
            )
        }
    }

    /// Release local bytes for assets confirmed durable.
    ///
    /// **Throws for any asset that is not durable**, and releases none of them.
    /// A non-durable verdict never triggers a destructive action: the client
    /// keeps the copy, retries with backoff, and says "not yet confirmed on
    /// server". Partial success here would be indistinguishable from data loss
    /// after the fact.
    public func releaseLocalCopies(for assetIDs: [AssetID]) async throws {
        let verdicts = try await verify(assetIDs: assetIDs, deep: false)
        let refused = verdicts.filter { !$0.authorisesRelease(at: configuration.clock.now) }
        guard refused.isEmpty else {
            throw CapsuleError(
                code: .uploadStorageInconsistent,
                detail: "CapsuleMock: \(refused.count) asset(s) are not confirmed durable"
            )
        }
        for assetID in assetIDs {
            guard let asset = await store.engine.asset(for: assetID) else { continue }
            await store.applyFetchOutcome(
                assetID,
                representations: asset.representations.removing(.original),
                state: .durable
            )
        }
    }

    /// Evict re-fetchable cached tiers.
    ///
    /// Never touches a device-owned original that has not been confirmed
    /// durable — those are excluded from ``LocalStorageBreakdown/reclaimableBytes``
    /// by construction, so the ceiling here cannot reach them however large the
    /// target is.
    public func evictCache(targetBytes: UInt64) async throws -> UInt64 {
        let available = try await localBreakdown().reclaimableBytes
        let freed = min(targetBytes, available)
        addReclaimed(freed)
        return freed
    }

    /// Pin an asset so it is exempt from cache eviction — offline access.
    public func setPinned(_ pinned: Bool, for assetIDs: [AssetID]) async throws {
        var current = self.pinned
        if pinned { current.formUnion(assetIDs) } else { current.subtract(assetIDs) }
        updatePinned(current)
    }

    // MARK: Derivation

    /// The per-tier byte totals.
    ///
    /// Derivatives are sized as fractions of the original rather than derived
    /// independently, because that ratio is what makes the breakdown legible:
    /// it shows that evicting thumbnails saves almost nothing while releasing
    /// confirmed-durable originals saves nearly everything, which is the whole
    /// point of splitting the number up.
    private static func makeBreakdown(
        library: MockLibrary,
        engine: MockQueryEngine,
        reclaimed: UInt64
    ) -> LocalStorageBreakdown {
        var byTier: [RepresentationTier: UInt64] = [:]
        var trashBytes: UInt64 = 0
        var unreleased: UInt64 = 0
        let ceiling = min(library.assetCount, 60000)
        for index in 0 ..< ceiling {
            let ref = MockAssetRef(kind: .live, index: index)
            let asset = engine.resolve(ref)
            let size = library.byteSize(for: ref, contentType: asset.contentType)
            for tier in asset.representations.heldTiers {
                byTier[tier, default: 0] += fraction(of: size, tier: tier)
            }
            if case .awaitingOriginal = asset.syncState { unreleased += size }
        }
        for ordinal in 0 ..< library.profile.derivedTrashCount {
            let ref = MockAssetRef(kind: .trashed, index: ordinal)
            trashBytes += library.byteSize(for: ref, contentType: library.contentType(for: ref))
        }
        let originals = byTier[.original] ?? 0
        byTier[.original] = originals - min(originals, reclaimed)
        return LocalStorageBreakdown(
            bytesByTier: byTier,
            trashBytes: trashBytes,
            unreleasedOriginalBytes: unreleased,
            availableDiskBytes: 92 * 1073741824
        )
    }

    private static func fraction(of size: UInt64, tier: RepresentationTier) -> UInt64 {
        switch tier {
        case .dominantColour: 0
        case .lqip: 220
        case .thumbnail: size / 220
        case .preview: size / 18
        case .original: size
        }
    }

    private static func verdicts(
        assetID: AssetID,
        seed: UInt64,
        isDurable: Bool,
        deep: Bool
    ) -> [BlobVerdict] {
        let roles: [BlobRole] = deep ? [.original, .metadata, .provenance, .derivative] : [.original, .metadata]
        return roles.enumerated().map { position, role in
            BlobVerdict(
                hash: MockIdentifiers.blobHash(
                    seed: seed,
                    ordinal: (MockAssetRef.decode(assetID)?.derivationIndex ?? 0) &* 4 &+ position
                ),
                role: role,
                stored: isDurable,
                indexed: isDurable,
                retrievable: isDurable
            )
        }
    }
}
