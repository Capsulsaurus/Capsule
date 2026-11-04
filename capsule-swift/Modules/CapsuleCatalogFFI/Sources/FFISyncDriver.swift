import CapsuleFoundation
import Foundation
import os

/// The sync driver's log: one INFO line per drain with its tally, DEBUG for
/// each page and each entry's verdict, WARNING for a quarantined entry.
private let syncLog = Logger(subsystem: CapsuleLog.subsystem, category: "sync-driver")

// MARK: - Session seam

/// The session and workspace verbs a sync of the managed lane drives, bound
/// to one server: `FfiSession.syncPull`, `FfiSession.fetchBlobToPath`,
/// `FfiWorkspace.applySyncEntryFromFile`, `FfiWorkspace.provenanceHead` and
/// `FfiWorkspace.assetIds`, and nothing else of either.
///
/// Narrowed to a protocol so the driver is tested over a scripted feed and
/// scripted verdicts rather than a live session.
protocol FFISyncDriverVerbs: Sendable {
    /// One page of the sync feed after `cursor` (empty for the feed's start).
    func syncPull(cursor: Data, pageSize: UInt32) async throws -> FfiSyncPage
    /// Fetch the blob at content address `hash` (`size` bytes) into the file
    /// at `path`, which holds exactly the verified ciphertext once this returns.
    func fetchBlob(hash: String, size: UInt64, to path: URL) async throws
    /// Run the sync chokepoint over `head` with the original's ciphertext read
    /// from the file at `path`. Writes nothing.
    func applySyncEntry(_ head: FfiSyncHead, ciphertextAt path: URL) throws -> FfiSyncApplyOutcome
    /// The head of a held asset's chain, as canonical CBOR.
    func provenanceHead(assetId: String) throws -> Data
    /// Every asset the workspace holds, live or in the trash.
    func assetIds() throws -> [String]
}

/// The verbs over a signed-in session and the workspace it syncs into.
struct FFISessionSyncVerbs: FFISyncDriverVerbs {
    let session: FfiSession
    let workspace: FfiWorkspace
    let apiBaseURL: String

    /// The root the blob route (`{root}/blob/{hash}`) hangs off: the server's
    /// `/v1` root on the origin `apiBaseURL` names, which the feed is pulled
    /// from too.
    static func blobBaseURL(apiBaseURL: String) -> String {
        let origin = apiBaseURL.hasSuffix("/") ? String(apiBaseURL.dropLast()) : apiBaseURL
        return origin + "/v1"
    }

    func syncPull(cursor: Data, pageSize: UInt32) async throws -> FfiSyncPage {
        try await session.syncPull(apiBaseUrl: apiBaseURL, cursor: cursor, pageSize: pageSize)
    }

    func fetchBlob(hash: String, size: UInt64, to path: URL) async throws {
        try await session.fetchBlobToPath(
            blobBaseUrl: Self.blobBaseURL(apiBaseURL: apiBaseURL),
            hash: hash,
            size: size,
            path: path.path
        )
    }

    func applySyncEntry(_ head: FfiSyncHead, ciphertextAt path: URL) throws -> FfiSyncApplyOutcome {
        try workspace.applySyncEntryFromFile(entry: head, ciphertextPath: path.path)
    }

    func provenanceHead(assetId: String) throws -> Data {
        try workspace.provenanceHead(assetId: assetId)
    }

    func assetIds() throws -> [String] {
        try workspace.assetIds()
    }
}

// MARK: - FFISyncDriver

/// The sync of the signed-in managed lane: drains `FfiSession.syncPull` from
/// a persisted cursor, runs every entry through the sync chokepoint
/// (`FfiWorkspace.applySyncEntryFromFile`), and keeps the cursor only once
/// the whole page is applied.
///
/// Each entry is judged against the head this device holds for its asset:
/// the head the last applied entry established, else the head of the
/// workspace's own chain. An entry whose record is that head is level and
/// costs nothing; any other has its metadata blob and original fetched into
/// the staging directory, is applied with that head as its local chain head,
/// and its staged files are removed; each drain first empties the staging
/// directory of what a drain the process was killed in left. An applied
/// entry's provenance head is kept for the asset's next entry. A quarantined
/// entry is logged and passed; an entry whose original the server does not hold yet is passed, since the
/// feed lists the asset again once it lands; a pending one (its epoch key not
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

    private let verbs: any FFISyncDriverVerbs
    private let store: any FFISyncStateStoring
    private let stagingDirectory: URL
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
    /// its fetches under `directory`, and hand each drain's tally to
    /// `drained`.
    public init(
        session: FfiSession,
        workspace: FfiWorkspace,
        apiBaseURL: String,
        directory: URL,
        drained: @escaping @Sendable (FFISyncDrain) async -> Void
    ) {
        self.init(
            verbs: FFISessionSyncVerbs(session: session, workspace: workspace, apiBaseURL: apiBaseURL),
            store: FFISyncStateFile(url: directory.appending(path: "state.json")),
            stagingDirectory: directory.appending(path: "staging", directoryHint: .isDirectory),
            drained: drained
        )
    }

    /// Sync through `verbs`, keeping state in `store`. The seam tests drive.
    init(
        verbs: any FFISyncDriverVerbs,
        store: any FFISyncStateStoring,
        stagingDirectory: URL,
        drained: @escaping @Sendable (FFISyncDrain) async -> Void
    ) {
        self.verbs = verbs
        self.store = store
        self.stagingDirectory = stagingDirectory
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
                try await apply(entry, into: &fold, heldIDs: heldIDs)
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
            syncLog.debug(
                "sync page \(tally.pages, privacy: .public) kept: \(page.entries.count, privacy: .public) entries, more=\(page.hasMore, privacy: .public)"
            )
            guard page.hasMore else { return }
        }
    }

    /// Judge one entry against the head this device holds for its asset, and
    /// apply it when it is not level.
    private func apply(_ entry: FfiFeedEntry, into fold: inout PageFold, heldIDs: [String: String]) async throws {
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
        guard entry.originalHeld, let original = entry.blobs.original else {
            syncLog.debug("entry of \(key, privacy: .public) awaits its original on the server")
            fold.tally.awaitingOriginal += 1
            return
        }
        guard let outcome = try await verify(entry, key: key, original: original, localChainHead: applied ?? own) else {
            fold.tally.skipped += 1
            return
        }
        switch outcome {
        case let .applied(facts):
            syncLog.debug("entry of \(key, privacy: .public) applied: \(facts.action, privacy: .public)")
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

    /// Fetch `entry`'s blobs into the staging directory and run the
    /// chokepoint over them, removing them after. `nil`, logged, for an entry
    /// naming a metadata blob it does not list, which cannot be fetched
    /// without its size; a `delete` or `trash-restore` names none.
    private func verify(
        _ entry: FfiFeedEntry,
        key: String,
        original: FfiBlobRef,
        localChainHead: Data?
    ) async throws -> FfiSyncApplyOutcome? {
        var metadata = Data()
        if !entry.metadataBlob.isEmpty {
            let address = String(data: entry.metadataBlob, encoding: .utf8)
            guard let blob = entry.blobs.derivatives.first(where: { $0.ciphertextHash == address }) else {
                syncLog.error("entry of \(key, privacy: .public) names a metadata blob it does not list")
                return nil
            }
            let staged = try await fetchStaged(blob)
            defer { removeStaged(staged) }
            metadata = try Data(contentsOf: staged)
        }
        let staged = try await fetchStaged(original)
        defer { removeStaged(staged) }
        let head = FfiSyncHead(
            albumId: entry.albumId,
            manifestCbor: entry.manifestCbor,
            metadataBlob: metadata,
            localChainHead: localChainHead
        )
        return try verbs.applySyncEntry(head, ciphertextAt: staged)
    }

    /// Fetch `blob` into a fresh file in the staging directory.
    private func fetchStaged(_ blob: FfiBlobRef) async throws -> URL {
        let staged = stagingDirectory.appending(path: UUID().uuidString)
        do {
            try await verbs.fetchBlob(hash: blob.ciphertextHash, size: blob.size, to: staged)
        } catch {
            removeStaged(staged)
            throw error
        }
        return staged
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

    private func removeStaged(_ url: URL) {
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
