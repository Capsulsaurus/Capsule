import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

// MARK: - PeoplePort

extension MockIntelligenceStore: PeoplePort {
    /// Every cluster, most populous first.
    ///
    /// **Stale clusters are included**, flagged rather than omitted: silently
    /// hiding a named person because their slot's canonical model changed is
    /// worse than showing them as pending regeneration, and it would look
    /// exactly like the app losing data.
    public func clusters(offset: Int, limit: Int) async throws -> Page<PersonCluster> {
        let request = PageRequest(offset: offset, limit: limit)
        let all = allClusters()
        return Page(
            items: MockQueryEngine.window(all, request: request),
            request: request,
            totalCount: all.count
        )
    }

    public func cluster(_ identifier: PersonID) async throws -> PersonCluster? {
        allClusters().first { $0.id == identifier }
    }

    /// The assets in a cluster.
    ///
    /// Derived membership is arithmetic — index ≡ residue (mod stride) — so the
    /// *n*-th member is computed rather than searched for, and a person's photos
    /// page as cheaply as the timeline does even at 250 000 assets.
    public func assets(in identifier: PersonID, offset: Int, limit: Int) async throws -> Page<LibraryAsset> {
        let request = PageRequest(offset: offset, limit: limit)
        let identifiers = memberIdentifiers(of: identifier)
        let window = MockQueryEngine.window(identifiers, request: request)
        let assets = try await libraryStore.assets(for: window)
        return Page(items: assets, request: request, totalCount: identifiers.count)
    }

    /// Name a cluster — an LWW write, so naming the same person on two devices
    /// converges instead of raising a conflict.
    public func setName(_ name: String?, for identifier: PersonID) async throws {
        let existing = self.name(for: identifier) ?? Lww()
        guard let name else {
            updateName(Lww(current: nil, superseded: existing.superseded), for: identifier)
            await peopleChanges.send(())
            return
        }
        let stamped = Stamped(
            value: name,
            timestamp: configurationClock.now,
            author: MockTagIdentity.authoringDevice(seed: seed)
        )
        updateName(existing.applying(stamped), for: identifier)
        await peopleChanges.send(())
    }

    /// Merge clusters that are the same person.
    ///
    /// Valid **only within one model slot**. Merging across slots would be the
    /// cross-model comparison the containment rule forbids, so it is refused
    /// rather than quietly allowed — the whole point of scoping cluster identity
    /// to a slot is that identities from two slots are not comparable at all.
    public func merge(_ ids: [PersonID], into target: PersonID) async throws {
        let all = allClusters()
        guard let destination = all.first(where: { $0.id == target }) else {
            throw CapsuleError(code: .albumInvalidID, detail: "CapsuleMock: unknown target cluster")
        }
        for identifier in ids where identifier != target {
            guard let source = all.first(where: { $0.id == identifier }) else { continue }
            guard source.modelSlot == destination.modelSlot else {
                throw CapsuleError(
                    code: .uploadInvalidAction,
                    detail: "CapsuleMock: clusters from different model slots are not comparable"
                )
            }
            recordMerge(identifier, into: target)
        }
        await peopleChanges.send(())
    }

    /// Split assets out of a cluster the grouping got wrong.
    ///
    /// Recorded rather than computed: derived membership is arithmetic, and a
    /// user's correction is by definition not something an arithmetic rule
    /// predicted.
    public func split(assetIDs: [AssetID], from identifier: PersonID) async throws -> PersonID {
        let created = MockIdentifiers.personID(
            seed: seed,
            ordinal: MockPeople.clusterCount + splitClusterIDs.count + 1
        )
        recordSplit(assetIDs, from: identifier, into: created)
        await peopleChanges.send(())
        return created
    }

    /// Hide a cluster from the People surface. A view-layer choice; it removes
    /// nothing and the cluster keeps contributing to search and predicates.
    public func setHidden(_ hidden: Bool, for identifier: PersonID) async throws {
        updateHidden(hidden, for: identifier)
        await peopleChanges.send(())
    }

    public nonisolated func changes() -> AsyncStream<Void> {
        peopleChanges.subscribe()
    }
}
