import CapsuleCatalog
import CapsuleFoundation
import Foundation
import os

/// The purge delivery's log: one INFO line per delivery naming its asset and
/// what became of it, DEBUG for each feed page drained.
private let purgeLog = Logger(subsystem: CapsuleLog.subsystem, category: "purge-delivery")

// MARK: - Session seam

/// The session and workspace verbs a purge delivery drives, bound to one
/// server: `FfiSession.syncPull`, `FfiSession.deliverPurge` and
/// `FfiWorkspace.purgedAssetIds`, and nothing else of either.
///
/// Narrowed to a protocol so the delivery is tested over a scripted feed and
/// outcomes rather than a live session.
protocol FFIPurgeDeliveryVerbs: Sendable {
    /// One page of the sync feed after `cursor` (empty for the feed's start).
    func syncPull(cursor: Data, pageSize: UInt32) async throws -> FfiSyncPage
    /// Send what the server lacks of the purged asset's kept chain.
    func deliverPurge(assetId: String, serverHead: Data?) async throws -> FfiLifecycleOpOutcome
    /// Every asset the workspace has purged.
    func purgedAssetIds() throws -> [String]
}

/// The verbs over a signed-in session and the workspace it delivers from.
struct FFISessionPurgeVerbs: FFIPurgeDeliveryVerbs {
    let session: FfiSession
    let workspace: FfiWorkspace
    let apiBaseURL: String

    func syncPull(cursor: Data, pageSize: UInt32) async throws -> FfiSyncPage {
        try await session.syncPull(apiBaseUrl: apiBaseURL, cursor: cursor, pageSize: pageSize)
    }

    func deliverPurge(assetId: String, serverHead: Data?) async throws -> FfiLifecycleOpOutcome {
        try await session.deliverPurge(
            apiBaseUrl: apiBaseURL,
            workspace: workspace,
            assetId: assetId,
            serverHead: serverHead
        )
    }

    func purgedAssetIds() throws -> [String] {
        try workspace.purgedAssetIds()
    }
}

// MARK: - FFIPurgeDelivery

/// The Rust-backed ``PurgeDelivering``: `FfiSession.deliverPurge` with the
/// asset's head read off the sync feed.
///
/// The server never sees a `delete` for an asset it has not indexed, and the
/// evidence that it has is its own feed: the head passed with each delivery is
/// the `manifest_cbor` of the latest feed entry listing the asset, or `nil`
/// when no entry lists it, which sends nothing and answers
/// ``PurgeDelivery/held``.
///
/// The feed is drained, not sampled. The first delivery drains it from its
/// start; each later one resumes from the cursor the last drain ended at and
/// folds what changed since into the heads it kept, so every delivery reads
/// the head the server holds now at the cost of one page. Concurrent
/// deliveries share one drain, so two never fold the same page out of order.
/// The heads live in memory: a relaunch drains from the start again, and a
/// drain that fails keeps the cursor and heads from before it, so nothing it
/// read halfway is trusted.
public actor FFIPurgeDelivery: PurgeDelivering {
    /// Entries asked for per feed page.
    static let pageSize: UInt32 = 500

    private let verbs: any FFIPurgeDeliveryVerbs
    /// The cursor the last completed drain ended at; empty before the first.
    private var cursor = Data()
    /// Every asset the feed has listed, by ``feedKey(_:)``, with the
    /// `manifest_cbor` of its latest entry (empty for one that carries none).
    private var heads: [String: Data] = [:]
    /// The drain in flight, which a concurrent delivery awaits rather than
    /// starting a second one.
    private var drain: Task<Void, any Error>?

    /// Deliver purges kept by `workspace` on `session`, against the server at
    /// `apiBaseURL` (`http[s]://host:port`, the root `syncPull` takes).
    public init(session: FfiSession, workspace: FfiWorkspace, apiBaseURL: String) {
        self.init(verbs: FFISessionPurgeVerbs(session: session, workspace: workspace, apiBaseURL: apiBaseURL))
    }

    /// Deliver through `verbs`. The seam tests drive.
    init(verbs: any FFIPurgeDeliveryVerbs) {
        self.verbs = verbs
    }

    /// Bring the feed's heads up to date, then send what the server lacks of
    /// `assetID`'s kept chain.
    ///
    /// - Throws: the feed read's error, before anything is sent; the session's
    ///   `FfiError` (a refused session, an asset this workspace never purged).
    public func deliverPurge(assetID: String) async throws -> PurgeDelivery {
        try await drainFeed()
        // An id that is not a UUID is never listed, so it is held; the
        // session refuses it as a bad argument either way.
        let head = Self.feedKey(assetID).flatMap { heads[$0] }
        purgeLog.debug(
            "delivering purge of \(assetID, privacy: .public), indexed=\(head != nil, privacy: .public)"
        )
        let outcome = try await verbs.deliverPurge(assetId: assetID, serverHead: head)
        let delivery = PurgeDelivery(outcome)
        switch outcome {
        case .applied:
            purgeLog.info("purge of \(assetID, privacy: .public) applied")
        case .notIndexed:
            purgeLog.info("purge of \(assetID, privacy: .public) held: the feed does not list the asset")
        case let .failed(kind, code, permanent, _, _, detail):
            purgeLog.error(
                "purge of \(assetID, privacy: .public) failed: kind=\(kind, privacy: .public) code=\(code ?? "none", privacy: .public) permanent=\(permanent, privacy: .public) \(detail, privacy: .public)"
            )
        }
        return delivery
    }

    public func purgedAssetIDs() async throws -> [String] {
        try verbs.purgedAssetIds()
    }

    // MARK: Feed

    /// Drain the feed from ``cursor`` to its end, joining the drain already in
    /// flight when there is one.
    private func drainFeed() async throws {
        if let drain {
            purgeLog.trace("joining the feed drain in flight")
            return try await drain.value
        }
        let task = Task { try await self.pullToEnd() }
        drain = task
        defer { drain = nil }
        try await task.value
    }

    /// Pull pages from ``cursor`` until the feed has no more, then commit the
    /// heads and cursor they lead to. A page that fails commits nothing.
    ///
    /// - Throws: the pull's error; ``FeedDrainError/cursorStalled`` for a page
    ///   that claims more while handing back the cursor it was asked from,
    ///   which would otherwise pull the same page forever.
    private func pullToEnd() async throws {
        var position = cursor
        var folded = heads
        var pages = 0
        var entries = 0
        while true {
            let page = try await verbs.syncPull(cursor: position, pageSize: Self.pageSize)
            pages += 1
            entries += page.entries.count
            for entry in page.entries {
                guard let key = Self.feedKey(entry.assetId) else {
                    purgeLog.debug("feed entry names no asset id this library uses")
                    continue
                }
                folded[key] = entry.manifestCbor
            }
            purgeLog.debug("feed page \(pages): \(page.entries.count) entries, more=\(page.hasMore, privacy: .public)")
            guard page.hasMore else {
                position = page.nextCursor
                break
            }
            guard page.nextCursor != position else {
                purgeLog.error("feed drain stopped: a page claims more but did not advance the cursor")
                throw FeedDrainError.cursorStalled
            }
            position = page.nextCursor
        }
        cursor = position
        heads = folded
        purgeLog.debug("feed drained: \(pages) pages, \(entries) entries, \(folded.count) assets listed")
    }

    /// The key an asset is listed under: its UUID in lowercase hyphenated
    /// form, which is how the workspace names a purged asset. `nil` for an id
    /// that is not a UUID, which names no asset of this workspace.
    static func feedKey(_ id: String) -> String? {
        UUID(uuidString: id).map { $0.uuidString.lowercased() }
    }

    /// ``feedKey(_:)`` of a feed entry's asset id, which the feed carries as
    /// the UTF-8 bytes of its UUID string.
    static func feedKey(_ id: Data) -> String? {
        String(data: id, encoding: .utf8).flatMap { feedKey($0) }
    }
}

/// Why a feed drain stopped before the feed's end.
enum FeedDrainError: Error, Equatable {
    /// A page claimed more entries but handed back the cursor it was pulled
    /// from.
    case cursorStalled
}

// MARK: - Outcomes

extension PurgeDelivery {
    /// The delivery a session's lifecycle op outcome names: an applied op (or
    /// a replay) is delivered, an unindexed asset is held, and a failure keeps
    /// the session's judgement of whether a resend can only be refused again.
    init(_ outcome: FfiLifecycleOpOutcome) {
        switch outcome {
        case .applied: self = .delivered
        case .notIndexed: self = .held
        case let .failed(_, _, permanent, _, _, _): self = .failed(permanent: permanent)
        }
    }
}
