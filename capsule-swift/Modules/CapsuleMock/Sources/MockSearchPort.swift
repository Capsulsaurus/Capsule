import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

// MARK: - SearchPort

extension MockIntelligenceStore: SearchPort {
    /// How many indices one search will examine.
    ///
    /// A derived library has no inverted index, so search is a scan — and an
    /// unbounded scan over 250 000 assets on every keystroke is exactly the
    /// behaviour a search field must not have. The cap is the honest shape of
    /// that limitation: results are the best matches in the most recent window,
    /// and a real local index replaces it rather than raising it.
    public static var searchExaminationLimit: Int { 20000 }

    /// Run a search.
    ///
    /// A term over a model-scoped facet whose slot changed evaluates as
    /// **stale-excluded** rather than being compared across model versions, so
    /// results can legitimately shrink after a model upgrade — and a UI that
    /// treats a smaller result set as a bug will mislead the user.
    public func search(
        _ text: String,
        scope: SearchScope,
        offset: Int,
        limit: Int
    ) async throws -> Page<SearchResult> {
        let request = PageRequest(offset: offset, limit: limit)
        let needle = text.lowercased()
        guard !needle.isEmpty else { return Page(items: [], request: request, totalCount: 0) }
        recordSearch(text)
        let library = libraryStore.library
        let engine = await libraryStore.engine
        var hits: [SearchResult] = []
        let ceiling = min(library.assetCount, Self.searchExaminationLimit)
        for index in 0 ..< ceiling {
            let ref = MockAssetRef(kind: .live, index: index)
            guard !engine.isSuppressed(liveIndex: index) else { continue }
            guard let matched = match(needle: needle, scope: scope, library: library, ref: ref) else { continue }
            hits.append(SearchResult(
                asset: engine.resolve(ref),
                matchedScope: matched.scope,
                score: matched.score
            ))
            guard hits.count < request.offset + request.limit else { break }
        }
        return Page(
            items: MockQueryEngine.window(hits, request: request),
            request: request,
            totalCount: nil
        )
    }

    /// Completion candidates — tags, names, places.
    public func suggestions(for partial: String, limit: Int) async throws -> [String] {
        let needle = partial.lowercased()
        guard !needle.isEmpty else { return [] }
        let names = (0 ..< MockPeople.clusterCount)
            .compactMap { MockPeople.derivedName(seed: seed, ordinal: $0) }
        let places = (MockTables.trips.map(\.identifier) + [MockTables.home.identifier])
        let pool = MockTables.userTags + MockTables.aiTags + names + places
        var seen = Set<String>()
        let matches = pool
            .filter { $0.contains(needle) }
            .filter { seen.insert($0).inserted }
        return Array(matches.prefix(max(0, limit)))
    }

    /// This device's recent searches.
    ///
    /// **Local-only, never synced.** A search history is a far more sensitive
    /// record than the photographs it searched — it is a list of what someone
    /// went looking for — so it does not leave the device even to the user's own
    /// other devices.
    public func recentSearches() async throws -> [String] {
        recentSearchHistory
    }

    public func clearRecentSearches() async throws {
        clearSearches()
    }

    // MARK: Matching

    private func match(
        needle: String,
        scope: SearchScope,
        library: MockLibrary,
        ref: MockAssetRef
    ) -> (scope: SearchScope, score: Double)? {
        let derivation = ref.derivationIndex
        if scope.contains(.userText) {
            let tags = library.userTags(derivationIndex: derivation)
            if tags.contains(where: { $0.contains(needle) }) { return (.userText, 1) }
            if library.caption(derivationIndex: derivation)?.contains(needle) == true {
                return (.userText, 0.9)
            }
        }
        if scope.contains(.aiTags) {
            // Stale-slot tags are excluded rather than compared across versions.
            let current = library.aiTags(derivationIndex: derivation)
                .filter { $0.modelSlot == MockTables.sceneTaggingSlot }
            if current.contains(where: { $0.tag.contains(needle) }) { return (.aiTags, 0.72) }
        }
        if scope.contains(.people), ref.kind == .live {
            let named = MockPeople.clusters(seed: seed, containing: ref.index)
                .compactMap { MockPeople.derivedName(seed: seed, ordinal: $0) }
            if named.contains(where: { $0.contains(needle) }) { return (.people, 0.85) }
        }
        if scope.contains(.places) {
            let place = library.captureInstant(for: ref).trip ?? MockTables.home
            if place.identifier.contains(needle) { return (.places, 0.66) }
        }
        if scope.contains(.semantic), library.camera(derivationIndex: derivation).lens.lowercased().contains(needle) {
            return (.semantic, 0.4)
        }
        return nil
    }
}
