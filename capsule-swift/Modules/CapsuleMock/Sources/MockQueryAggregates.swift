import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - Reads

public extension MockQueryEngine {
    /// One window of the timeline.
    ///
    /// The walk stops as soon as the window is full, so a first page over a
    /// 250 000-asset library touches a few hundred indices rather than all of
    /// them. Deep offsets under a filter are linear in the offset — a real local
    /// index answers that with a covering index, and pretending otherwise here
    /// would hide the cost rather than model it.
    func page(matching query: TimelineQuery, offset: Int, limit: Int) -> Page<LibraryAsset> {
        let request = PageRequest(offset: offset, limit: limit)
        guard request.limit > 0 else {
            return Page(items: [], request: request, totalCount: cheapTotalCount(query))
        }
        if canUseDayBoundaryFastPath(query) {
            return fastPage(query: query, request: request)
        }
        var items: [LibraryAsset] = []
        items.reserveCapacity(min(request.limit, 512))
        var position = 0
        forEachMatch(query) { _, ref in
            defer { position += 1 }
            guard position >= request.offset else { return true }
            items.append(resolve(ref))
            return items.count < request.limit
        }
        return Page(items: items, request: request, totalCount: cheapTotalCount(query))
    }

    /// The unfiltered live window, read straight off the day boundary array.
    private func fastPage(query: TimelineQuery, request: PageRequest) -> Page<LibraryAsset> {
        let upper = min(library.assetCount, request.offset + request.limit)
        guard request.offset < upper else {
            return Page(items: [], request: request, totalCount: library.assetCount)
        }
        let items = (request.offset ..< upper).map { library.asset(at: $0) }
        return Page(items: items, request: request, totalCount: library.assetCount)
    }

    /// Per-day counts for the whole query, oldest day first.
    ///
    /// Empty days are omitted: a section is a header plus rows, and a header
    /// over nothing is a rendering artefact rather than information. The sum of
    /// what this returns is exactly what ``count(matching:)`` reports and
    /// exactly how many rows paging will hand back.
    func dayCounts(matching query: TimelineQuery) -> [DayCount] {
        if canUseDayBoundaryFastPath(query) {
            return library.unfilteredDayCounts()
        }
        var counts: [Int: Int] = [:]
        forEachMatch(query) { dayIndex, _ in
            counts[dayIndex, default: 0] += 1
            return true
        }
        return counts.keys.sorted(by: >).compactMap { dayIndex in
            guard let total = counts[dayIndex], total > 0 else { return nil }
            return DayCount(dayKey: library.dayKey(forDay: dayIndex), count: total)
        }
    }

    /// How many assets match, without the rows.
    func count(matching query: TimelineQuery) -> Int {
        if canUseDayBoundaryFastPath(query) { return library.assetCount }
        var total = 0
        forEachMatch(query) { _, _ in
            total += 1
            return true
        }
        return total
    }

    /// Resolve several assets in the order requested, missing ids omitted
    /// rather than represented by a placeholder.
    func assets(for identifiers: [AssetID]) -> [LibraryAsset] {
        identifiers.compactMap { asset(for: $0) }
    }

    /// The full asset for a ref, with the user's edits applied.
    func resolve(_ ref: MockAssetRef) -> LibraryAsset {
        let identifier = ref.identifier(seed: library.profile.seed)
        return patched(library.asset(for: ref), identifier: identifier)
    }

    /// A total only when producing one is cheap.
    ///
    /// `nil` means "unknown", never "zero" — a smart-album membership count is a
    /// full evaluation, and forcing every page to produce one would make paging
    /// pointless. A UI renders a count-less page rather than an empty one.
    private func cheapTotalCount(_ query: TimelineQuery) -> Int? {
        if canUseDayBoundaryFastPath(query) { return library.assetCount }
        guard query.slice != .live else { return nil }
        return asideEntries(query).count
    }

    // MARK: Trash

    /// The trash with each entry's signed retention deadline.
    ///
    /// The deadline is a cryptographic floor in the real system — signed into
    /// the `delete` manifest, so the server can neither accelerate nor delay it.
    /// Here it is simply derived from the delete instant and the album's
    /// retention policy, which is the same arithmetic a user sees counting down.
    func trashEntries(offset: Int, limit: Int, retentionDays: Int) -> Page<TrashEntry> {
        let request = PageRequest(offset: offset, limit: limit)
        let entries = asideEntries(TimelineQuery.trash).map { extra -> TrashEntry in
            let identifier = extra.ref.identifier(seed: library.profile.seed)
            let asset = resolve(extra.ref)
            let deletedAt = asset.deletedAt ?? CapsuleTimestamp(epochSeconds: extra.seconds)
            let patch = overlay.patch(for: identifier)
            let deadline = patch?.retentionUntil
                ?? CapsuleTimestamp(epochSeconds: deletedAt.epochSeconds + Int64(retentionDays) * 86400)
            return TrashEntry(
                assetID: extra.ref.uuidString(seed: library.profile.seed),
                deletedAt: deletedAt,
                retentionUntil: deadline
            )
        }
        let window = Self.window(entries, request: request)
        return Page(items: window, request: request, totalCount: entries.count)
    }

    /// Clamp a window to a collection, so an offset past the end is an empty
    /// final page rather than a crash.
    static func window<Element>(_ items: [Element], request: PageRequest) -> [Element] {
        guard request.limit > 0, request.offset < items.count else { return [] }
        return Array(items[request.offset ..< min(request.offset + request.limit, items.count)])
    }
}
