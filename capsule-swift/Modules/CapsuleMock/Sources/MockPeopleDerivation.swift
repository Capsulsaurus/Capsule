import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - Cluster derivation

extension MockIntelligenceStore {
    /// The clock every stamped edit in this store is authored against.
    var configurationClock: MockClock { storeConfiguration.clock }

    /// Every cluster the People surface lists, most populous first.
    ///
    /// Merged-away clusters are gone from the listing and their assets have
    /// moved to the target, which is what a merge means. Hidden ones stay
    /// listed with their flag set, because hiding is a view choice rather than a
    /// deletion and the surface that offers "unhide" needs something to offer.
    func allClusters() -> [PersonCluster] {
        let libraryCount = libraryStore.library.assetCount
        let merged = mergedAway
        var clusters: [PersonCluster] = []
        for ordinal in 0 ..< MockPeople.clusterCount {
            let identifier = MockIdentifiers.personID(seed: seed, ordinal: ordinal)
            guard !merged.contains(identifier) else { continue }
            clusters.append(derivedCluster(ordinal: ordinal, identifier: identifier, libraryCount: libraryCount))
        }
        clusters.append(contentsOf: splitClusterIDs.compactMap { identifier in
            guard let members = splitClusterMembers(identifier) else { return nil }
            return PersonCluster(
                id: identifier,
                name: name(for: identifier) ?? Lww(),
                keyAssetID: members.first,
                assetCount: members.count,
                modelSlot: MockTables.faceEmbeddingSlot,
                isHidden: isHidden(identifier)
            )
        })
        return clusters.sorted { lhs, rhs in
            lhs.assetCount == rhs.assetCount
                ? lhs.id.rawValue < rhs.id.rawValue
                : lhs.assetCount > rhs.assetCount
        }
    }

    private func derivedCluster(
        ordinal: Int,
        identifier: PersonID,
        libraryCount: Int
    ) -> PersonCluster {
        let absorbed = absorbedCounts(into: identifier, libraryCount: libraryCount)
        let own = MockPeople.assetCount(seed: seed, ordinal: ordinal, libraryCount: libraryCount)
        let removed = splitAwayAssets(from: identifier).count
        let derivedName = MockPeople.derivedName(seed: seed, ordinal: ordinal)
        let stored = name(for: identifier)
        return PersonCluster(
            id: identifier,
            name: stored ?? register(derivedName),
            keyAssetID: keyAsset(ordinal: ordinal, libraryCount: libraryCount),
            assetCount: max(0, own + absorbed - removed),
            modelSlot: MockTables.faceEmbeddingSlot,
            isStale: MockPeople.isStale(seed: seed, ordinal: ordinal),
            isHidden: isHidden(identifier) || MockPeople.isHidden(seed: seed, ordinal: ordinal)
        )
    }

    /// A name is never fabricated: an unnamed cluster gets a never-written
    /// register, not a placeholder, so "unnamed" and "named empty" stay
    /// distinguishable.
    private func register(_ name: String?) -> Lww<String> {
        guard let name else { return Lww() }
        return Lww(current: Stamped(
            value: name,
            timestamp: configurationClock.offset(days: -90),
            author: MockTagIdentity.authoringDevice(seed: seed)
        ))
    }

    private func absorbedCounts(into target: PersonID, libraryCount: Int) -> Int {
        var total = 0
        for ordinal in 0 ..< MockPeople.clusterCount {
            let identifier = MockIdentifiers.personID(seed: seed, ordinal: ordinal)
            guard mergeTarget(of: identifier) == target else { continue }
            total += MockPeople.assetCount(seed: seed, ordinal: ordinal, libraryCount: libraryCount)
        }
        return total
    }

    private func keyAsset(ordinal: Int, libraryCount: Int) -> AssetID? {
        let index = MockPeople.memberIndex(seed: seed, ordinal: ordinal, position: 0)
        guard index < libraryCount else { return nil }
        return libraryStore.library.identifier(at: index)
    }

    /// Every asset in a cluster, newest first.
    ///
    /// The arithmetic sequences of the cluster and everything merged into it,
    /// interleaved by index — which is newest-first order, because the base
    /// index *is* the timeline.
    func memberIdentifiers(of identifier: PersonID) -> [AssetID] {
        if let members = splitClusterMembers(identifier) { return members }
        let library = libraryStore.library
        let removed = splitAwayAssets(from: identifier)
        var ordinals: [Int] = []
        for ordinal in 0 ..< MockPeople.clusterCount {
            let candidate = MockIdentifiers.personID(seed: seed, ordinal: ordinal)
            if candidate == identifier || mergeTarget(of: candidate) == identifier {
                ordinals.append(ordinal)
            }
        }
        var indices: [Int] = []
        for ordinal in ordinals {
            let rule = MockPeople.membership(seed: seed, ordinal: ordinal)
            indices.append(contentsOf: stride(from: rule.residue, to: library.assetCount, by: rule.stride))
        }
        return indices.sorted()
            .map { library.identifier(at: $0) }
            .filter { !removed.contains($0) }
    }
}
