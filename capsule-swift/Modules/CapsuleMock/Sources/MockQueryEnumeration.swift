import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - Enumeration

/// The one traversal every read goes through.
///
/// Paging and the day aggregate are the *same* walk — one collecting, one
/// counting — so they cannot disagree about what a query selects. That
/// agreement is the whole contract: a virtualized grid sizes its sections from
/// `dayCounts(...)` and then fetches rows by offset, and if the two ever
/// disagree the grid shows the wrong photo under the wrong header.
extension MockQueryEngine {
    /// A trashed or hidden asset the user brought back, which is therefore live
    /// without being in the base index.
    struct Extra: Sendable {
        var dayIndex: Int
        var seconds: Int64
        var ref: MockAssetRef
    }

    /// Refs whose derived population says one thing and whose patch says
    /// another — a restored trash entry, an unhidden asset, a member the user
    /// pulled out of its stack.
    func liveExtras() -> [Extra] {
        var extras: [Extra] = []
        for (identifier, patch) in overlay.patches {
            guard !patch.isPurged, let ref = MockAssetRef.decode(identifier), library.contains(ref) else { continue }
            guard isLiveByPatch(ref: ref, patch: patch) else { continue }
            let instant = library.captureInstant(for: ref)
            extras.append(Extra(dayIndex: instant.dayIndex, seconds: instant.utcSeconds, ref: ref))
        }
        return extras.sorted { $0.seconds > $1.seconds }
    }

    /// Whether a non-live-kind ref has been patched back into the timeline.
    private func isLiveByPatch(ref: MockAssetRef, patch: MockAssetPatch) -> Bool {
        switch ref.kind {
        case .live:
            false
        case .trashed:
            patch.isDeleted == false
        case .userHidden:
            patch.isUserHidden == false
        case .stackMember:
            patch.stackMembership == .cleared
        }
    }

    /// Whether the fast path is available: an unfiltered live query over an
    /// unedited visibility state, which is answered from the day boundary array
    /// alone and never touches an asset.
    func canUseDayBoundaryFastPath(_ query: TimelineQuery) -> Bool {
        guard query.slice == .live, !query.includeStackHidden, hasNoFacetFilter(query) else { return false }
        return !overlay.patches.values.contains { patch in
            patch.isPurged || patch.isDeleted != nil || patch.isUserHidden != nil
                || patch.stackMembership == .cleared
        }
    }

    /// Walk every ref the query selects, newest first. Returning `false` from
    /// `body` stops the walk.
    func forEachMatch(_ query: TimelineQuery, _ body: (Int, MockAssetRef) -> Bool) {
        switch query.slice {
        case .live:
            forEachLiveMatch(query, body)
        case .trash, .userHidden:
            for entry in asideEntries(query) where !body(entry.dayIndex, entry.ref) { return }
        }
    }

    private func forEachLiveMatch(_ query: TimelineQuery, _ body: (Int, MockAssetRef) -> Bool) {
        let extras = liveExtras()
        var cursor = 0
        for dayIndex in 0 ..< library.dayCount {
            let range = library.indexRange(forDay: dayIndex)
            for index in range {
                let seconds = extras.isEmpty ? 0 : library.captureInstant(for: MockAssetRef(kind: .live, index: index)).utcSeconds
                while cursor < extras.count, extras[cursor].seconds > seconds {
                    guard emit(extras[cursor], query: query, body: body) else { return }
                    cursor += 1
                }
                guard emitLive(dayIndex: dayIndex, index: index, query: query, body: body) else { return }
            }
            while cursor < extras.count, extras[cursor].dayIndex == dayIndex {
                guard emit(extras[cursor], query: query, body: body) else { return }
                cursor += 1
            }
        }
        while cursor < extras.count {
            guard emit(extras[cursor], query: query, body: body) else { return }
            cursor += 1
        }
    }

    private func emit(_ extra: Extra, query: TimelineQuery, body: (Int, MockAssetRef) -> Bool) -> Bool {
        emitIfMatching(dayIndex: extra.dayIndex, ref: extra.ref, query: query, body: body)
    }

    /// One base index, plus its collapsed stack members when the query expands
    /// stacks.
    private func emitLive(
        dayIndex: Int,
        index: Int,
        query: TimelineQuery,
        body: (Int, MockAssetRef) -> Bool
    ) -> Bool {
        guard !isSuppressed(liveIndex: index) else { return true }
        let primary = MockAssetRef(kind: .live, index: index)
        guard emitIfMatching(dayIndex: dayIndex, ref: primary, query: query, body: body) else { return false }
        guard query.includeStackHidden else { return true }
        for ref in library.stackRefs(at: index).dropFirst() {
            guard emitIfMatching(dayIndex: dayIndex, ref: ref, query: query, body: body) else { return false }
        }
        return true
    }

    private func emitIfMatching(
        dayIndex: Int,
        ref: MockAssetRef,
        query: TimelineQuery,
        body: (Int, MockAssetRef) -> Bool
    ) -> Bool {
        let identifier = ref.identifier(seed: library.profile.seed)
        let patch = overlay.patch(for: identifier)
        guard patch?.isPurged != true else { return true }
        guard matchesFacets(query, facets: library.facets(for: ref), patch: patch) else { return true }
        return body(dayIndex, ref)
    }

    // MARK: Trash and Hidden

    /// The trash and hidden populations, materialized.
    ///
    /// Bounded by construction — a trash view is a list a person reads, and the
    /// derived populations are tens of entries — so materializing here costs
    /// nothing and buys a straightforward sort.
    func asideEntries(_ query: TimelineQuery) -> [Extra] {
        var entries: [Extra] = []
        let derivedKind: MockAssetRef.Kind = query.slice == .trash ? .trashed : .userHidden
        let derivedCount = query.slice == .trash
            ? library.profile.derivedTrashCount
            : library.profile.derivedHiddenCount
        for ordinal in 0 ..< derivedCount {
            let ref = MockAssetRef(kind: derivedKind, index: ordinal)
            guard belongsToAside(query, ref: ref) else { continue }
            entries.append(makeExtra(ref))
        }
        for (identifier, patch) in overlay.patches {
            guard !patch.isPurged, let ref = MockAssetRef.decode(identifier), library.contains(ref) else { continue }
            guard ref.kind != derivedKind, patchPlacesInAside(query, patch: patch) else { continue }
            entries.append(makeExtra(ref))
        }
        return entries
            .filter { matchesFacets(query, facets: library.facets(for: $0.ref), patch: overlay.patch(for: $0.ref.identifier(seed: library.profile.seed))) }
            .sorted { lhs, rhs in
                lhs.seconds == rhs.seconds
                    ? lhs.ref.uuidString(seed: library.profile.seed) < rhs.ref.uuidString(seed: library.profile.seed)
                    : lhs.seconds > rhs.seconds
            }
    }

    private func belongsToAside(_ query: TimelineQuery, ref: MockAssetRef) -> Bool {
        let patch = overlay.patch(for: ref.identifier(seed: library.profile.seed))
        guard patch?.isPurged != true else { return false }
        if query.slice == .trash { return patch?.isDeleted != false }
        return patch?.isUserHidden != false && patch?.isDeleted != true
    }

    private func patchPlacesInAside(_ query: TimelineQuery, patch: MockAssetPatch) -> Bool {
        query.slice == .trash
            ? patch.isDeleted == true
            : patch.isUserHidden == true && patch.isDeleted != true
    }

    private func makeExtra(_ ref: MockAssetRef) -> Extra {
        let instant = library.captureInstant(for: ref)
        return Extra(dayIndex: instant.dayIndex, seconds: instant.utcSeconds, ref: ref)
    }
}
