import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

// MARK: - MockSmartAlbumPortAdapter

/// ``SmartAlbumPort`` over ``MockLibraryStore``.
///
/// A thin adapter rather than another conformance on the actor, for a blunt
/// reason: ``AlbumPort`` and ``SmartAlbumPort`` both declare
/// `changes() -> AsyncStream<Void>`, so one type cannot provide two distinct
/// streams for them. Since a smart-album edit and a container-album edit are
/// genuinely different events — one is a settings-document write, the other an
/// MLS commit — collapsing them onto one stream would make every album screen
/// re-read on every predicate keystroke.
public struct MockSmartAlbumPortAdapter: SmartAlbumPort {
    let store: MockLibraryStore

    public init(store: MockLibraryStore) {
        self.store = store
    }

    /// Every definition, **including ones this build cannot evaluate**.
    ///
    /// A definition ahead of this build's grammar is returned with
    /// ``SmartAlbumDefinition/isEvaluable`` false and preserved verbatim. Hiding
    /// it would be the never-strip rule broken; evaluating it partially would
    /// show a different album here than on the device that wrote it. So it is
    /// listed, and the UI says "created by a newer app version".
    public func definitions() async throws -> [SmartAlbumDefinition] {
        await store.smartAlbumList
    }

    public func definition(_ identifier: SmartAlbumID) async throws -> SmartAlbumDefinition? {
        await store.smartAlbum(identifier)
    }

    /// Create or replace a definition.
    ///
    /// Validated through ``PredicateValidator`` before writing: an invalid
    /// predicate is a **structural rejection**, never a tolerated definition,
    /// because a predicate that evaluated differently on two devices would show
    /// two different albums under one name.
    public func save(_ definition: SmartAlbumDefinition) async throws {
        try definition.validate()
        await store.putSmartAlbum(definition)
        await store.smartAlbumChanges.send(())
    }

    public func delete(_ identifier: SmartAlbumID) async throws {
        await store.removeSmartAlbum(identifier)
        await store.smartAlbumChanges.send(())
    }

    /// Evaluate a definition into a page.
    ///
    /// A pure function of `(definition, decryptable assets)`, so the same window
    /// is identical on every device. Evaluation walks the timeline in its
    /// canonical order rather than re-sorting, which is what keeps that promise
    /// without materializing the library.
    public func evaluate(_ identifier: SmartAlbumID, offset: Int, limit: Int) async throws -> Page<LibraryAsset> {
        guard let definition = await store.smartAlbum(identifier) else {
            return Page(items: [], request: PageRequest(offset: offset, limit: limit))
        }
        guard definition.isEvaluable else {
            throw CapsuleError(
                code: .protocolVersionUnsupported,
                detail: "CapsuleMock: predicate schema \(definition.predicateSchema) is ahead of this build"
            )
        }
        return await store.evaluate(definition, offset: offset, limit: limit)
    }

    /// Preview a predicate without saving it — what the editor's live result
    /// count reads.
    public func preview(_ predicate: SmartAlbumPredicate, limit: Int) async throws -> Page<LibraryAsset> {
        try PredicateValidator.validate(predicate)
        let definition = SmartAlbumDefinition(
            smartAlbumID: SmartAlbumID("preview"),
            displayName: Lww(),
            predicate: predicate
        )
        return await store.evaluate(definition, offset: 0, limit: limit)
    }

    public func changes() -> AsyncStream<Void> {
        store.smartAlbumChanges.subscribe()
    }
}

// MARK: - Evaluation

extension MockLibraryStore {
    /// How many matches a non-default sort will collect before it stops.
    ///
    /// A rating sort cannot be answered by walking the timeline, so it collects
    /// and then orders — which is bounded rather than unbounded, because a
    /// 250 000-row materialization is exactly what this module exists to avoid.
    static var reorderingCeiling: Int { 50_000 }

    /// Walk the live timeline applying the predicate, stopping once the window
    /// is full.
    ///
    /// The sort is honoured by walking the underlying order for the default
    /// `(capture_timestamp, desc)` spec and by sorting the collected window
    /// otherwise. That is an honest limitation of a derived library: a rating
    /// sort over 250 000 assets needs an index, and the mock does not pretend to
    /// have one — it collects, then orders.
    func evaluate(_ definition: SmartAlbumDefinition, offset: Int, limit: Int) -> Page<LibraryAsset> {
        let request = PageRequest(offset: offset, limit: limit)
        let engine = self.engine
        let sort = definition.effectiveSort
        let isNaturalOrder = sort == .default
        var matched: [LibraryAsset] = []
        var position = 0
        engine.forEachMatch(.default) { _, ref in
            guard MockPredicateEvaluator.matches(
                definition.predicate,
                context: self.context(for: ref, engine: engine)
            ) else { return true }
            defer { position += 1 }
            guard !isNaturalOrder || position >= request.offset else { return true }
            matched.append(engine.resolve(ref))
            return isNaturalOrder
                ? matched.count < request.limit
                : matched.count < Self.reorderingCeiling
        }
        guard !isNaturalOrder else {
            return Page(items: matched, request: request, totalCount: nil)
        }
        let ordered = MockSortOrder.sorted(matched, by: sort)
        return Page(
            items: MockQueryEngine.window(ordered, request: request),
            request: request,
            totalCount: ordered.count
        )
    }

    /// Everything a predicate term can read about one asset.
    func context(for ref: MockAssetRef, engine: MockQueryEngine) -> MockPredicateContext {
        let asset = engine.resolve(ref)
        let clusters = ref.kind == .live
            ? MockPeople.clusters(seed: configuration.seed, containing: ref.index)
            : []
        return MockPredicateContext(
            asset: asset,
            gps: currentOverlay.patch(for: asset.id)?.geolocation.applied(to: library.geolocation(for: ref))
                ?? library.geolocation(for: ref),
            hasCameraIdentity: asset.contentType.mediaKind == .image,
            personIDs: Set(clusters.map {
                MockIdentifiers.personID(seed: configuration.seed, ordinal: $0).rawValue
            })
        )
    }
}

// MARK: - MockSortOrder

/// The closed sort key set, applied to a collected window.
public enum MockSortOrder {
    /// Sort ascending and reverse, rather than negating the comparator.
    ///
    /// Negating `<` yields `>=`, which is not a strict weak ordering and makes
    /// `sorted(by:)` undefined behaviour. Reversing is the boring correct thing.
    public static func sorted(_ assets: [LibraryAsset], by spec: SortSpec) -> [LibraryAsset] {
        let ascending = assets.sorted { compare($0, $1, key: spec.key) }
        return spec.direction == .ascending ? ascending : Array(ascending.reversed())
    }

    private static func compare(_ lhs: LibraryAsset, _ rhs: LibraryAsset, key: SortSpec.Key) -> Bool {
        switch key {
        case .captureTimestamp:
            lhs.effectiveCaptureTimestamp < rhs.effectiveCaptureTimestamp
        case .importTimestamp:
            lhs.importTimestamp < rhs.importTimestamp
        case .rating:
            lhs.rating < rhs.rating
        case .unknown:
            lhs.stableSortKey < rhs.stableSortKey
        }
    }
}
