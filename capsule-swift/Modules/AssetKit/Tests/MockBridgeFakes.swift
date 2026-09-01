import Foundation

import AssetKit
import CapsuleDomain
import CapsuleFoundation
import CapsulePorts

// MARK: - StreamHub

/// Fans one notification out to every held stream.
///
/// A local stand-in rather than `CapsuleMock`'s broadcaster: these suites test
/// the bridge against the *port contracts*, so pulling in the mock world would
/// make a failure ambiguous between the adapter and the mock.
actor StreamHub<Element: Sendable> {
    private var continuations: [Int: AsyncStream<Element>.Continuation] = [:]
    private var nextToken = 0

    /// How many streams are registered. The tests poll this before mutating,
    /// because registration hops onto the actor and a change emitted before it
    /// lands is legitimately missed.
    var subscriberCount: Int { continuations.count }

    nonisolated func subscribe() -> AsyncStream<Element> {
        AsyncStream(bufferingPolicy: .bufferingNewest(32)) { continuation in
            Task { await register(continuation) }
        }
    }

    func send(_ element: Element) {
        for continuation in continuations.values {
            continuation.yield(element)
        }
    }

    private func register(_ continuation: AsyncStream<Element>.Continuation) {
        continuations[nextToken] = continuation
        nextToken += 1
    }
}

// MARK: - FakeLibrary

/// A small, honest library behind ``LibraryPort`` and ``OrganizePort``.
///
/// It stores whole ``LibraryAsset`` rows and applies the real
/// ``TimelineQuery/admitsVisibility(of:)``, so a slice the adapter asks for is
/// the slice the domain defines rather than one this fake invented.
actor FakeLibrary: LibraryPort, OrganizePort {
    /// The device every add id in this fake is issued by.
    static let device = DeviceID("00000000-0000-4000-8000-0000000000fa")
    static let session = SessionID("00000000-0000-4000-8000-0000000000fb")

    nonisolated let hub = StreamHub<LibraryChange>()

    private var rows: [AssetID: LibraryAsset]
    private var order: [AssetID]
    private var tagAdds: [AssetID: [AddID: String]] = [:]
    private var counter: UInt64 = 0

    /// Every read this fake has served, so a test can assert on what the
    /// adapter actually asked for.
    private(set) var pageRequests: [PageRequest] = []
    private(set) var callCount = 0

    init(assets: [LibraryAsset]) {
        rows = Dictionary(uniqueKeysWithValues: assets.map { ($0.id, $0) })
        order = assets.map(\.id)
    }

    var subscriberCount: Int {
        get async { await hub.subscriberCount }
    }

    // MARK: LibraryPort

    func assets(matching query: TimelineQuery, offset: Int, limit: Int) async throws -> Page<LibraryAsset> {
        callCount += 1
        let request = PageRequest(offset: offset, limit: limit)
        pageRequests.append(request)
        let matched = matching(query)
        let start = min(max(0, offset), matched.count)
        let end = min(start + limit, matched.count)
        return Page(items: Array(matched[start ..< end]), request: request, totalCount: matched.count)
    }

    func dayCounts(matching query: TimelineQuery) async throws -> [DayCount] {
        callCount += 1
        var totals: [DayKey: Int] = [:]
        for asset in matching(query) {
            totals[asset.dayKey, default: 0] += 1
        }
        return totals.keys.sorted().map { DayCount(dayKey: $0, count: totals[$0] ?? 0) }
    }

    func assetCount(matching query: TimelineQuery) async throws -> Int {
        callCount += 1
        return matching(query).count
    }

    func asset(for id: AssetID) async throws -> LibraryAsset? {
        callCount += 1
        return rows[id]
    }

    func assets(for ids: [AssetID]) async throws -> [LibraryAsset] {
        callCount += 1
        return ids.compactMap { rows[$0] }
    }

    /// A sidecar carrying the OR-set entries this fake has issued, so an
    /// un-favourite can find the add id that introduced the tag.
    func sidecar(for id: AssetID) async throws -> SidecarV1? {
        callCount += 1
        guard let asset = rows[id] else { return nil }
        var tags = OrSet<String>()
        for (addID, tag) in tagAdds[id] ?? [:] {
            tags = tags.adding(tag, addID: addID)
        }
        return SidecarV1(
            cryptoSuiteID: 1,
            uuid: asset.stableSortKey,
            hash: "fake",
            captureTimestamp: asset.captureTime.captureTimestamp,
            importTimestamp: asset.importTimestamp,
            contentType: asset.contentType,
            dimensions: asset.dimensions,
            tagsUser: tags,
            deviceID: Self.device,
            sessionID: Self.session,
            gps: gpsFixes[id]
        )
    }

    func provenanceChain(for _: AssetID) async throws -> [ProvenanceRecord] {
        callCount += 1
        return []
    }

    nonisolated func changes() -> AsyncStream<LibraryChange> {
        hub.subscribe()
    }

    // MARK: OrganizePort — the writes the bridge performs

    func setRating(_ rating: UInt8, for assetIDs: [AssetID]) async throws {
        try await edit(assetIDs) { $0.rating = min(5, rating) }
    }

    func setHidden(_ hidden: Bool, for assetIDs: [AssetID]) async throws {
        try await edit(assetIDs) { $0.isUserHidden = hidden }
        await hub.send(.reload)
    }

    func addUserTag(_ tag: String, to assetIDs: [AssetID]) async throws {
        for id in assetIDs {
            counter += 1
            tagAdds[id, default: [:]][AddID(deviceID: Self.device, counter: counter)] = tag
        }
        try await edit(assetIDs) { $0.tagsUser.insert(tag) }
    }

    func removeUserTag(addID: AddID, from assetID: AssetID) async throws {
        guard let tag = tagAdds[assetID]?[addID] else { throw UnobservedRemove(addID: addID) }
        tagAdds[assetID]?[addID] = nil
        let stillPresent = tagAdds[assetID]?.values.contains(tag) ?? false
        try await edit([assetID]) { asset in
            if !stillPresent { asset.tagsUser.remove(tag) }
        }
    }

    func moveToTrash(_ assetIDs: [AssetID], retentionDays _: Int?) async throws {
        try await edit(assetIDs) { asset in
            asset.isDeleted = true
            asset.deletedAt = asset.importTimestamp
        }
        await hub.send(.reload)
    }

    func restoreFromTrash(_ assetIDs: [AssetID]) async throws {
        try await edit(assetIDs) { asset in
            asset.isDeleted = false
            asset.deletedAt = nil
        }
        await hub.send(.reload)
    }

    func purge(_ assetIDs: [AssetID]) async throws {
        for id in assetIDs {
            rows[id] = nil
            order.removeAll { $0 == id }
        }
        await hub.send(.reload)
    }

    func trashEntries(offset: Int, limit: Int) async throws -> Page<TrashEntry> {
        let entries = matching(.trash).map { asset in
            TrashEntry(
                assetID: asset.stableSortKey,
                deletedAt: asset.deletedAt ?? asset.importTimestamp,
                retentionUntil: asset.importTimestamp
            )
        }
        let request = PageRequest(offset: offset, limit: limit)
        let start = min(max(0, offset), entries.count)
        let end = min(start + limit, entries.count)
        return Page(items: Array(entries[start ..< end]), request: request, totalCount: entries.count)
    }

    // MARK: OrganizePort — unexercised surface

    func setCull(_ flag: CullFlag, for assetIDs: [AssetID]) async throws {
        try await edit(assetIDs) { $0.cull = flag }
    }

    func promoteAITag(addID: AddID, on _: AssetID, alsoRemoveFromAI _: Bool) async throws {
        throw UnobservedRemove(addID: addID)
    }

    func dismissAITag(addID: AddID, on _: AssetID) async throws {
        throw UnobservedRemove(addID: addID)
    }

    func setCaption(_ caption: String?, for assetID: AssetID) async throws {
        try await edit([assetID]) { $0.caption = caption }
    }

    func restoreCaption(_ superseded: Stamped<String>, for assetID: AssetID) async throws {
        try await edit([assetID]) { $0.caption = superseded.value }
    }

    func setGps(_ gps: Gps?, for assetID: AssetID) async throws {
        gpsFixes[assetID] = gps
        await hub.send(.assetsChanged(dayKeys: []))
    }

    // MARK: Private

    private var gpsFixes: [AssetID: Gps] = [:]

    private func matching(_ query: TimelineQuery) -> [LibraryAsset] {
        order.compactMap { rows[$0] }
            .filter { query.admitsVisibility(of: $0) }
            .sorted(by: LibraryAsset.isOrderedNewestFirst)
    }

    private func edit(_ ids: [AssetID], _ body: (inout LibraryAsset) -> Void) async throws {
        var days = Set<DayKey>()
        for id in ids {
            guard var asset = rows[id] else { continue }
            body(&asset)
            rows[id] = asset
            days.insert(asset.dayKey)
        }
        await hub.send(.assetsChanged(dayKeys: days))
    }
}

// MARK: - SyntheticLibrary

/// A library that is **described** rather than stored.
///
/// The scale test needs a 250 000-asset timeline without allocating 250 000
/// structs, and describing one is also the sharper assertion: this fake can
/// only answer a window that was actually requested, so "the snapshot did not
/// materialise the library" is observable as a row count rather than inferred.
actor SyntheticLibrary: LibraryPort {
    /// Midnight UTC on the newest day, 2026-08-22.
    static let newestDayEpoch: Int64 = 1787356800

    let totalAssets: Int
    let assetsPerDay: Int

    private(set) var pageRequests: [PageRequest] = []
    private(set) var rowsFetched = 0

    init(totalAssets: Int, assetsPerDay: Int) {
        self.totalAssets = max(0, totalAssets)
        self.assetsPerDay = max(1, assetsPerDay)
    }

    /// The row a timeline index describes: newest first, `assetsPerDay` to a
    /// day, each one second earlier than the last within its day.
    func row(at index: Int) -> LibraryAsset {
        let dayOffset = Int64(index / assetsPerDay)
        let withinDay = Int64(index % assetsPerDay)
        let seconds = Self.newestDayEpoch - dayOffset * 86400 + 43200 - withinDay
        let timestamp = CapsuleTimestamp(epochSeconds: seconds)
        return LibraryAsset(
            id: .managed(uuid: "synthetic-\(index)"),
            mediaType: .photo,
            contentType: .heic,
            captureTime: CaptureTime(captureTimestamp: timestamp),
            importTimestamp: timestamp,
            dimensions: Dimensions(width: 4000, height: 3000)
        )
    }

    /// The UTC day index `index` sections into.
    func dayKey(at index: Int) -> DayKey {
        DayKey(epochSeconds: Self.newestDayEpoch - Int64(index / assetsPerDay) * 86400)
    }

    func assets(matching _: TimelineQuery, offset: Int, limit: Int) async throws -> Page<LibraryAsset> {
        let request = PageRequest(offset: offset, limit: limit)
        pageRequests.append(request)
        let start = min(max(0, offset), totalAssets)
        let end = min(start + limit, totalAssets)
        rowsFetched += end - start
        return Page(items: (start ..< end).map(row(at:)), request: request, totalCount: totalAssets)
    }

    /// The day histogram, oldest day first — the aggregate the port promises a
    /// virtualized grid, and here the only read that costs anything.
    func dayCounts(matching _: TimelineQuery) async throws -> [DayCount] {
        let fullDays = totalAssets / assetsPerDay
        let remainder = totalAssets % assetsPerDay
        var counts: [DayCount] = []
        if remainder > 0 {
            counts.append(DayCount(dayKey: dayKey(at: totalAssets - 1), count: remainder))
        }
        for day in stride(from: fullDays - 1, through: 0, by: -1) {
            counts.append(DayCount(dayKey: dayKey(at: day * assetsPerDay), count: assetsPerDay))
        }
        return counts
    }

    func assetCount(matching _: TimelineQuery) async throws -> Int {
        totalAssets
    }

    func asset(for _: AssetID) async throws -> LibraryAsset? { nil }
    func assets(for _: [AssetID]) async throws -> [LibraryAsset] { [] }
    func sidecar(for _: AssetID) async throws -> SidecarV1? { nil }
    func provenanceChain(for _: AssetID) async throws -> [ProvenanceRecord] { [] }
    nonisolated func changes() -> AsyncStream<LibraryChange> { AsyncStream { $0.finish() } }
}
