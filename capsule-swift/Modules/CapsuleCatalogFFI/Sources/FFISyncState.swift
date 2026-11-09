import Foundation

// MARK: - Persisted state

/// What a sync keeps between drains: the cursor the last applied page ended
/// at, and the record hash of the head each asset's applied entry
/// established, which is handed back to the chokepoint as the asset's local
/// chain head for its next entry.
struct FFISyncState: Codable, Equatable, Sendable {
    /// The cursor to pull the next page from; empty for the feed's start.
    var cursor = Data()
    /// The 32-byte provenance head per listed asset, keyed by
    /// ``FFIPurgeDelivery/feedKey(_:)``.
    var heads: [String: Data] = [:]
}

/// Where ``FFISyncState`` lives between drains and launches.
protocol FFISyncStateStoring: Sendable {
    /// The state the last applied page saved; `nil` before the first.
    func load() throws -> FFISyncState?
    /// Replace the saved state with `state`, all at once.
    func save(_ state: FFISyncState) throws
}

/// ``FFISyncStateStoring`` in one JSON file, replaced atomically on each save
/// so a crash mid-write leaves the state of the page before it.
struct FFISyncStateFile: FFISyncStateStoring {
    let url: URL

    func load() throws -> FFISyncState? {
        guard FileManager.default.fileExists(atPath: url.path) else { return nil }
        return try JSONDecoder().decode(FFISyncState.self, from: Data(contentsOf: url))
    }

    func save(_ state: FFISyncState) throws {
        try FileManager.default.createDirectory(
            at: url.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        try JSONEncoder().encode(state).write(to: url, options: .atomic)
    }
}

// MARK: - Drain

/// What one ``FFISyncDriver/sync()`` did, counted over the pages it kept.
public struct FFISyncDrain: Sendable, Equatable {
    /// The assets the drain listed that the feed had not listed before it, in
    /// the order the feed first listed them, as the workspace names them.
    public var newlyIndexed: [String] = []
    /// Pages applied, each one's cursor kept.
    public var pages = 0
    /// Entries the chokepoint verified and applied.
    public var applied = 0
    /// Entries whose record is the head this device already holds, so nothing
    /// was fetched or applied.
    public var level = 0
    /// Entries the chokepoint refused for good.
    public var quarantined = 0
    /// Entries whose original the server does not hold yet, left for the entry
    /// that lists it once it lands.
    public var awaitingOriginal = 0
    /// Entries that name no asset of this library, carry no record, name a
    /// metadata blob address that is not a content address, or are the
    /// tombstone of an asset this device does not hold whose records the
    /// server no longer serves (its retention window closed, a takedown, a
    /// purge).
    public var skipped = 0
    /// Whether the server had moved the library to a new sync epoch, so the
    /// drain started over from the feed's start.
    public var resynced = false

    /// Fold a kept page's tally into the drain's.
    mutating func add(_ page: FFISyncDrain) {
        newlyIndexed += page.newlyIndexed
        pages += 1
        applied += page.applied
        level += page.level
        quarantined += page.quarantined
        awaitingOriginal += page.awaitingOriginal
        skipped += page.skipped
    }
}

/// Why a sync stopped before the feed's end.
enum FFISyncError: Error, Equatable {
    /// The chokepoint holds an entry whose epoch key has not arrived here
    /// yet. Its page is not applied, so the next sync pulls it again.
    case pending(assetID: String, reason: String)
}
