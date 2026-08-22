import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

// MARK: - LibraryPort

extension MockLibraryStore: LibraryPort {
    /// One window of the timeline.
    ///
    /// Not gated by ``MockGate``: a gallery read is local, and the offline-first
    /// contract is that it never attempts the network. Making it fail under
    /// ``MockScenario/offline`` would model a product Capsule is not.
    public func assets(matching query: TimelineQuery, offset: Int, limit: Int) async throws -> Page<LibraryAsset> {
        engine.page(matching: query, offset: offset, limit: limit)
    }

    public func dayCounts(matching query: TimelineQuery) async throws -> [DayCount] {
        engine.dayCounts(matching: query)
    }

    public func assetCount(matching query: TimelineQuery) async throws -> Int {
        engine.count(matching: query)
    }

    public func asset(for identifier: AssetID) async throws -> LibraryAsset? {
        engine.asset(for: identifier)
    }

    public func assets(for identifiers: [AssetID]) async throws -> [LibraryAsset] {
        engine.assets(for: identifiers)
    }

    public func sidecar(for identifier: AssetID) async throws -> SidecarV1? {
        guard let ref = MockAssetRef.decode(identifier), library.contains(ref) else { return nil }
        return MockSidecarFactory.sidecar(
            library: library,
            ref: ref,
            patch: currentOverlay.patch(for: identifier),
            clock: configuration.clock
        )
    }

    /// The provenance chain, oldest first.
    ///
    /// Present even for a purged asset. The bytes go; the audit trail does not —
    /// so the activity view can always answer "what happened to this photo",
    /// including for one that no longer exists.
    public func provenanceChain(for identifier: AssetID) async throws -> [ProvenanceRecord] {
        guard let ref = MockAssetRef.decode(identifier), library.contains(ref) else { return [] }
        return MockSidecarFactory.provenanceChain(
            library: library,
            ref: ref,
            patch: currentOverlay.patch(for: identifier),
            clock: configuration.clock
        )
    }

    public nonisolated func changes() -> AsyncStream<LibraryChange> {
        libraryChanges.subscribe()
    }
}

// MARK: - StackPort

extension MockLibraryStore: StackPort {
    /// One stack, with its **derived** group cull state.
    ///
    /// Built from the resolved assets rather than the raw derivation, so a flag
    /// the user just set is reflected here. A group has no stored flag of its
    /// own — its state is computed from its members every time — and reading the
    /// pre-edit derivation would reintroduce exactly the second source of truth
    /// that rule exists to prevent.
    public func stack(_ identifier: StackID) async throws -> Stack? {
        guard let index = library.stackIndex(of: identifier),
              let type = library.stackType(at: index)
        else { return nil }
        let engine = self.engine
        let refs = library.stackRefs(at: index)
        let members = refs.map { (ref: $0, asset: engine.resolve($0)) }
        let primary = members.first { $0.asset.isStackCover } ?? members[0]
        return Stack(
            id: identifier,
            stackType: type,
            primaryAssetID: primary.ref.uuidString(seed: configuration.seed),
            memberAssetIDs: members.map { $0.ref.uuidString(seed: configuration.seed) },
            cullState: GroupCullState(members: members.map { $0.asset.cull })
        )
    }

    public func members(of identifier: StackID) async throws -> [LibraryAsset] {
        guard let index = library.stackIndex(of: identifier) else { return [] }
        let engine = self.engine
        return library.stackRefs(at: index).map { engine.resolve($0) }
    }

    /// Group assets into a new stack.
    ///
    /// Metadata-only, like everything on this port: it rewrites each member's
    /// `stack_membership` register and nothing else. No bytes move, so nothing
    /// here can lose an original — which is why even choosing a burst's best
    /// photo is a pointer change.
    public func createStack(
        from assetIDs: [AssetID],
        type: StackType,
        primary: AssetID
    ) async throws -> StackID {
        try type.requireWritable()
        let ordinal = 900_000 + currentOverlay.patches.count
        let identifier = MockIdentifiers.stackID(seed: configuration.seed, ordinal: ordinal)
        for (position, assetID) in assetIDs.enumerated() {
            let isPrimary = assetID == primary
            await mutate(assetID) { patch in
                patch.stackMembership = .set(StackMembership(
                    stackID: identifier,
                    stackType: type,
                    role: isPrimary ? .primary : .member,
                    memberIndex: UInt32(position)
                ))
            }
        }
        return identifier
    }

    public func addToStack(_ assetID: AssetID, stackID: StackID, role: StackRole) async throws {
        try role.requireWritable()
        guard let index = library.stackIndex(of: stackID), let type = library.stackType(at: index) else {
            throw CapsuleError(code: .albumInvalidID, detail: "CapsuleMock: unknown stack")
        }
        let ordinal = UInt32(library.stackMemberCount(at: index))
        await mutate(assetID) { patch in
            patch.stackMembership = .set(StackMembership(
                stackID: stackID,
                stackType: type,
                role: role,
                memberIndex: ordinal
            ))
        }
    }

    /// Leave a stack — a stamped `nil`, which converges with a concurrent stack
    /// edit from another device and is distinct from never having been stacked.
    public func removeFromStack(_ assetID: AssetID) async throws {
        await mutate(assetID) { $0.stackMembership = .cleared }
        await announceReload()
    }

    public func setPrimary(_ assetID: AssetID, in stackID: StackID) async throws {
        guard let index = library.stackIndex(of: stackID), let type = library.stackType(at: index) else {
            throw CapsuleError(code: .albumInvalidID, detail: "CapsuleMock: unknown stack")
        }
        let engine = self.engine
        for ref in library.stackRefs(at: index) {
            let identifier = ref.identifier(seed: configuration.seed)
            let existing = engine.resolve(ref).stackMembership
            let isPrimary = identifier == assetID
            await mutate(identifier) { patch in
                patch.stackMembership = .set(StackMembership(
                    stackID: stackID,
                    stackType: type,
                    role: isPrimary ? .primary : .member,
                    memberIndex: existing?.memberIndex
                ))
            }
        }
        await announceReload()
    }

    public func unstack(_ identifier: StackID) async throws {
        guard let index = library.stackIndex(of: identifier) else { return }
        let identifiers = library.stackRefs(at: index).map { $0.identifier(seed: configuration.seed) }
        await mutate(identifiers) { $0.stackMembership = .cleared }
        await announceReload()
    }
}
