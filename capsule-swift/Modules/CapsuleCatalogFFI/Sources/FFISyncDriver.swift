import CapsuleFoundation
import Foundation
import os

/// The sync driver's log: one INFO line per drain with its tally, DEBUG for
/// each page and each entry's verdict, WARNING for a quarantined entry.
private let syncLog = Logger(subsystem: CapsuleLog.subsystem, category: "sync-driver")

// MARK: - FFISyncDriver

/// The sync of the signed-in managed lane: drains `FfiSession.syncPull` from
/// a persisted cursor, runs every entry through the sync chokepoint, writes
/// each applied one into the managed catalog, and keeps the cursor only once
/// the whole page is applied and written.
///
/// Each entry is judged against the head this device holds for its asset:
/// the head the last applied entry established, else the head of the
/// workspace's own chain. An entry whose record is that head is level and
/// costs nothing; any other has its original fetched into the staging
/// directory, and the metadata blob it names fetched by address without a
/// size (`FfiSession.fetchMetadataBlob`, as the feed lists none), and is
/// applied with that head as its local chain head. An entry that names a
/// metadata blob is opened
/// (`FfiWorkspace.openSyncedOriginal`): verified, and its original decrypted
/// to a staged plaintext that hashes to the signed sidecar's digest, which
/// the catalog writer keeps in the store. An opened entry of an asset the
/// workspace does not hold first has its earlier records fetched
/// (`FfiSession.fetchProvenanceHistory`); when there are any, the asset was
/// first seen past its create, and the entry is opened against them
/// (`FfiWorkspace.openSyncedHistory`), which verifies the chain from its
/// create and adopts the asset, so its trash verbs and tag edits work here.
/// A `delete` in trash names its asset's live metadata blob, so one of an
/// asset first seen at its delete is adopted into this device's trash and
/// can be restored. An adopted asset is held for the rest of the drain. An
/// entry of an asset the workspace does not hold that names no metadata blob
/// (an album page's tombstone, which names no blob and no held original
/// either) is adopted from its records alone: its history is fetched, the
/// live metadata blob and the original are named from it
/// (`syncedHistorySources`), both are fetched, and the entry is opened as
/// one naming them is. A history whose records name no live metadata blob,
/// or do not decode, quarantines the entry as
/// ``SyncVerdict/unboundHistoryReason``, fetching no blob. A tombstone of an
/// asset the workspace does not hold, on an album page or the owner's own
/// feed, whose delete's signed window had closed at the feed's `changed_at`
/// (`syncedRetentionClosed`) is passed without a fetch, and so is one whose
/// records or blobs the server refuses for good (`error.blob.gone`: a closed
/// window, a takedown, a purge), as is any records-only adoption refused so:
/// the server will never serve them again, and passing the delete of an
/// asset this device does not hold loses nothing. A
/// history fetch that fails otherwise is thrown as a blob fetch is; one whose
/// records do not decode, or run past the walk's bound, quarantines the entry
/// as ``SyncVerdict/malformedHistoryReason``. An album page's tombstone of a
/// held asset, which names no original, is applied against the held chain
/// and the original it commits to (`FfiWorkspace.applyHeldSyncEntry`),
/// fetching nothing, so the asset moves to this device's trash. Any other
/// entry of a held asset that names no metadata blob is only verified
/// (`FfiWorkspace.applySyncEntryFromFile`).
/// Every staged file the writer does not keep is removed; each drain first
/// empties the staging directory of what a drain the process was killed in
/// left. An applied entry's facts go to the catalog writer, and its
/// provenance head is kept for the asset's next entry. A quarantined entry,
/// or an opened one whose original does not decrypt or match its digest, is
/// logged and passed and writes nothing; an entry whose original the server
/// does not hold yet is passed, writing nothing, since the feed lists the
/// asset again once it lands; a pending one (its epoch key not
/// here yet) stops the drain before its page is kept, so the next sync
/// retries it. A page that fails for any reason keeps the cursor and heads
/// from before it, so nothing it applied halfway is trusted.
///
/// After every drain, failed or not, `drained` is handed the tally of the
/// pages it kept, whose ``FFISyncDrain/newlyIndexed`` lists the assets those
/// pages listed that the feed had not listed before: the evidence that can
/// release a purge, delete or restore held while the server had not indexed
/// its asset. "Before" is every asset a kept page has listed in this process,
/// every asset with a kept head, and the assets earlier in the same drain.
///
/// A sync asked for while one runs joins it rather than starting a second, so
/// two never apply the same page.
public actor FFISyncDriver {
    /// Entries asked for per feed page.
    static let pageSize: UInt32 = 500
    /// The code `FfiError.Sync` carries when the server moved the library to
    /// a new sync epoch (`capsule_i18n::error_codes::SYNC_CURSOR_STALE`).
    static let staleCursorCode = "error.sync.cursor_stale"
    /// The code `FfiError.Fetch` carries for a blob or record the server will
    /// never serve again: a `410` past a retention window or under a
    /// takedown, or a `404` (`capsule_i18n::error_codes::BLOB_GONE`).
    static let blobGoneCode = "error.blob.gone"

    /// The session seam and the staging directory are internal, not private,
    /// so the judging of one entry (`FFISyncDriverJudging.swift`) can fetch
    /// and stage through them.
    let verbs: any FFISyncDriverVerbs
    private let store: any FFISyncStateStoring
    let stagingDirectory: URL
    private let catalog: any FFISyncCatalogWriting
    private let drained: @Sendable (FFISyncDrain) async -> Void
    /// The state the last kept page saved, loaded on the first drain.
    private var state: FFISyncState?
    /// Every asset a kept page has listed since this driver started.
    private var listed: Set<String> = []
    /// The drain in flight, which a concurrent sync joins.
    private var drain: Task<FFISyncDrain, any Error>?
    /// How many syncs joined a drain in flight rather than starting one.
    private(set) var joins = 0

    /// Sync the lane `workspace` holds over `session` against the server at
    /// `apiBaseURL` (`http[s]://host:port`), keeping its state and staging
    /// its fetches under `directory`, writing each applied entry through
    /// `catalog`, and hand each drain's tally to `drained`. `directory` must
    /// be on the volume `catalog` keeps originals on, so a verified original
    /// is moved into the store rather than copied.
    public init(
        session: FfiSession,
        workspace: FfiWorkspace,
        apiBaseURL: String,
        directory: URL,
        catalog: FFISyncCatalogWriter,
        drained: @escaping @Sendable (FFISyncDrain) async -> Void
    ) {
        self.init(
            verbs: FFISessionSyncVerbs(session: session, workspace: workspace, apiBaseURL: apiBaseURL),
            store: FFISyncStateFile(url: directory.appending(path: "state.json")),
            stagingDirectory: directory.appending(path: "staging", directoryHint: .isDirectory),
            catalog: catalog,
            drained: drained
        )
    }

    /// Sync through `verbs`, keeping state in `store` and writing through
    /// `catalog`. The seam tests drive.
    init(
        verbs: any FFISyncDriverVerbs,
        store: any FFISyncStateStoring,
        stagingDirectory: URL,
        catalog: any FFISyncCatalogWriting,
        drained: @escaping @Sendable (FFISyncDrain) async -> Void
    ) {
        self.verbs = verbs
        self.store = store
        self.stagingDirectory = stagingDirectory
        self.catalog = catalog
        self.drained = drained
    }

    /// Drain the feed from the kept cursor to its end, joining the drain in
    /// flight when there is one.
    ///
    /// - Throws: the pull's, a fetch's or the workspace's error, or
    ///   ``FFISyncError/pending(assetID:reason:)``, each after `drained` has
    ///   been handed what the pages before it kept;
    ///   `FeedDrainError.cursorStalled` for a page that claims more without
    ///   advancing the cursor.
    @discardableResult
    public func sync() async throws -> FFISyncDrain {
        if let drain {
            syncLog.trace("joining the sync drain in flight")
            joins += 1
            return try await drain.value
        }
        let task = Task { try await self.drainFeed() }
        drain = task
        defer { drain = nil }
        return try await task.value
    }

    /// One drain: pages applied and kept one at a time, then the tally handed
    /// to ``drained``, also when a page fails.
    private func drainFeed() async throws -> FFISyncDrain {
        var tally = FFISyncDrain()
        do {
            try await applyPages(into: &tally)
        } catch {
            syncLog.error(
                "sync drain stopped after \(tally.pages, privacy: .public) pages: \(String(describing: error), privacy: .public)"
            )
            await drained(tally)
            throw error
        }
        syncLog.info(
            "sync drained: pages=\(tally.pages, privacy: .public) applied=\(tally.applied, privacy: .public) level=\(tally.level, privacy: .public) quarantined=\(tally.quarantined, privacy: .public) awaiting=\(tally.awaitingOriginal, privacy: .public) skipped=\(tally.skipped, privacy: .public) newly_indexed=\(tally.newlyIndexed.count, privacy: .public) resynced=\(tally.resynced, privacy: .public)"
        )
        await drained(tally)
        return tally
    }

    private func applyPages(into tally: inout FFISyncDrain) async throws {
        var kept = loadState()
        try clearStaging()
        var heldIDs: [String: String] = [:]
        for id in try verbs.assetIds() {
            if let key = FFIPurgeDelivery.feedKey(id) { heldIDs[key] = id }
        }
        var known = listed.union(kept.heads.keys)
        while true {
            let page: FfiSyncPage
            do {
                page = try await verbs.syncPull(cursor: kept.cursor, pageSize: Self.pageSize)
            } catch let error where Self.isStaleCursor(error) && !tally.resynced {
                syncLog.info("the server moved this library to a new sync epoch; resyncing from the start")
                kept.cursor = Data()
                tally.resynced = true
                continue
            }
            var fold = PageFold(state: kept, known: known)
            for entry in page.entries {
                try await apply(entry, into: &fold, heldIDs: &heldIDs)
            }
            if page.hasMore, page.nextCursor == kept.cursor {
                syncLog.error("sync drain stopped: a page claims more but did not advance the cursor")
                throw FeedDrainError.cursorStalled
            }
            fold.state.cursor = page.nextCursor
            try store.save(fold.state)
            kept = fold.state
            state = fold.state
            known = fold.known
            listed.formUnion(fold.listed)
            tally.add(fold.tally)
            let pagesSoFar = tally.pages
            syncLog.debug(
                "sync page \(pagesSoFar, privacy: .public) kept: \(page.entries.count, privacy: .public) entries, more=\(page.hasMore, privacy: .public)"
            )
            guard page.hasMore else { return }
        }
    }

    /// Judge one entry against the head this device holds for its asset, and
    /// apply it when it is not level. An asset the workspace adopts while it
    /// is applied joins `heldIDs`: the adoption is the workspace's own write,
    /// kept whether or not the page is.
    private func apply(_ entry: FfiFeedEntry, into fold: inout PageFold, heldIDs: inout [String: String]) async throws {
        guard let key = FFIPurgeDelivery.feedKey(entry.assetId) else {
            syncLog.debug("feed entry names no asset id this library uses")
            fold.tally.skipped += 1
            return
        }
        fold.listed.insert(key)
        if fold.known.insert(key).inserted {
            fold.tally.newlyIndexed.append(key)
        }
        guard !entry.manifestCbor.isEmpty else {
            syncLog.debug("entry of \(key, privacy: .public) carries no record")
            fold.tally.skipped += 1
            return
        }
        let record = FFIPurgeDelivery.headHash(entry.manifestCbor)
        let applied = fold.state.heads[key]
        let own = try heldIDs[key].map { try FFIPurgeDelivery.headHash(verbs.provenanceHead(assetId: $0)) }
        if record == applied || record == own {
            syncLog.trace("entry of \(key, privacy: .public) is level with the head held here")
            fold.state.heads[key] = record
            fold.tally.level += 1
            return
        }
        let verdict: SyncVerdict?
        switch try await judge(entry, key: key, held: heldIDs[key] != nil, localChainHead: applied ?? own) {
        case .awaitingOriginal:
            fold.tally.awaitingOriginal += 1
            return
        case let .judged(judged):
            verdict = judged
        }
        guard let verdict else {
            fold.tally.skipped += 1
            return
        }
        defer {
            if let plaintext = verdict.plaintext { removeStaged(plaintext) }
        }
        if case let .applied(facts) = verdict.outcome, facts.adopted {
            heldIDs[key] = facts.assetId
        }
        try await settle(verdict, key: key, into: &fold)
    }

    /// Fold the chokepoint's verdict on the entry of `key` into the page,
    /// writing an applied one into the catalog first.
    private func settle(_ verdict: SyncVerdict, key: String, into fold: inout PageFold) async throws {
        switch verdict.outcome {
        case let .applied(facts):
            let write = try await catalog.write(facts, original: verdict.plaintext)
            syncLog.debug(
                "entry of \(key, privacy: .public) applied: \(facts.action, privacy: .public), catalog \(String(describing: write), privacy: .public)"
            )
            fold.state.heads[key] = facts.provenanceHead
            fold.tally.applied += 1
        case let .pending(reason):
            syncLog.info("entry of \(key, privacy: .public) held pending: \(reason, privacy: .public)")
            throw FFISyncError.pending(assetID: key, reason: reason)
        case let .quarantined(reason, detail):
            syncLog.warning(
                "entry of \(key, privacy: .public) quarantined: \(reason, privacy: .public) \(detail ?? "", privacy: .public)"
            )
            fold.tally.quarantined += 1
        }
    }

    /// Empty the staging directory, creating it where it is missing. A drain
    /// runs this before its first fetch: no other drain is in flight then, so
    /// whatever the directory holds was left by one the process was killed in.
    private func clearStaging() throws {
        let fileManager = FileManager.default
        if fileManager.fileExists(atPath: stagingDirectory.path) {
            let leftovers = try fileManager.contentsOfDirectory(at: stagingDirectory, includingPropertiesForKeys: nil)
            if !leftovers.isEmpty {
                syncLog.info("removing \(leftovers.count, privacy: .public) staged sync files a stopped drain left")
            }
            leftovers.forEach(removeStaged)
        }
        try fileManager.createDirectory(at: stagingDirectory, withIntermediateDirectories: true)
    }

    func removeStaged(_ url: URL) {
        guard FileManager.default.fileExists(atPath: url.path) else { return }
        do {
            try FileManager.default.removeItem(at: url)
        } catch {
            syncLog.error("staged sync file not removed: \(String(describing: error), privacy: .public)")
        }
    }

    /// The state kept by the last page, read once. Unreadable state is
    /// logged and dropped: the drain then starts at the feed's start with no
    /// heads, which re-verifies every entry rather than losing any.
    private func loadState() -> FFISyncState {
        if let state { return state }
        let loaded: FFISyncState
        do {
            loaded = try store.load() ?? FFISyncState()
        } catch {
            syncLog.error("sync state unreadable, starting from the feed's start: \(String(describing: error), privacy: .public)")
            loaded = FFISyncState()
        }
        state = loaded
        return loaded
    }

    /// The lowercase hex SHA-256 the feed's UTF-8 `address` bytes spell, or
    /// `nil` for bytes that spell none, which the blob route refuses.
    static func contentAddress(_ address: Data) -> String? {
        guard address.count == 64,
              address.allSatisfy({ (0x30 ... 0x39).contains($0) || (0x61 ... 0x66).contains($0) })
        else { return nil }
        return String(bytes: address, encoding: .utf8)
    }

    static func isStaleCursor(_ error: any Error) -> Bool {
        guard let error = error as? FfiError, case let .Sync(code, _) = error else { return false }
        return code == staleCursorCode
    }
}

/// One page's effect while its entries are applied, kept only once all are.
private struct PageFold {
    var state: FFISyncState
    /// The assets listed before this page or earlier in it.
    var known: Set<String>
    /// The assets this page lists.
    var listed: Set<String> = []
    var tally = FFISyncDrain()
}
