import CapsuleFoundation
import Foundation
import os

/// The sync driver's log, the same category ``FFISyncDriver``'s drain logs
/// to, so one entry's verdict reads as one stream.
private let syncLog = Logger(subsystem: CapsuleLog.subsystem, category: "sync-driver")

// MARK: - Judging one entry

/// How ``FFISyncDriver`` reaches the chokepoint's verdict on one entry that is
/// not level with the head held for its asset: the fetches it needs, and the
/// verify or open it runs over them.
extension FFISyncDriver {
    /// The chokepoint's verdict on `entry`, which is not level with the head
    /// held for its asset (`localChainHead`), by the path its asset and what
    /// it names take: adopted from its records, applied against the held
    /// chain, or fetched and verified or opened. `nil` inside `judged` for an
    /// entry passed without a verdict.
    func judge(
        _ entry: FfiFeedEntry,
        key: String,
        held: Bool,
        localChainHead: Data?
    ) async throws -> EntryJudgement {
        if !held, entry.metadataBlob.isEmpty {
            return try await .judged(adoptFromRecords(entry, key: key, localChainHead: localChainHead))
        }
        if held, entry.kind == .deleted, entry.metadataBlob.isEmpty, !entry.originalHeld || entry.blobs.original == nil {
            return try .judged(applyHeldTombstone(entry, key: key, localChainHead: localChainHead))
        }
        guard entry.originalHeld, let original = entry.blobs.original else {
            syncLog.debug("entry of \(key, privacy: .public) awaits its original on the server")
            return .awaitingOriginal
        }
        guard !held, entry.kind == .deleted else {
            return try await .judged(verify(entry, key: key, original: original, localChainHead: localChainHead, held: held))
        }
        return try await .judged(passingUnserved(entry, key: key) {
            try await verify(entry, key: key, original: original, localChainHead: localChainHead, held: held)
        })
    }

    /// Fetch `entry`'s blobs into the staging directory and run the
    /// chokepoint over them, removing the fetched ciphertext after. An entry
    /// naming a metadata blob is opened, and an `opened` verdict hands back
    /// the staged plaintext, which the caller removes once the catalog has
    /// written it; one naming none (of a `held` asset: an unheld one is
    /// ``adoptFromRecords(_:key:localChainHead:)``'s) is only verified. An
    /// opened entry of an asset not `held` here is opened against
    /// its fetched history when it has one. The feed names the metadata blob
    /// by address only, so it is fetched without a size
    /// (``FFISyncDriverVerbs/fetchMetadataBlob(hash:)``). `nil`, logged, for
    /// an entry whose metadata blob address is not a content address, which
    /// no blob route serves.
    private func verify(
        _ entry: FfiFeedEntry,
        key: String,
        original: FfiBlobRef,
        localChainHead: Data?,
        held: Bool
    ) async throws -> SyncVerdict? {
        var metadata = Data()
        var history: [Data] = []
        if !entry.metadataBlob.isEmpty {
            guard let address = Self.contentAddress(entry.metadataBlob) else {
                syncLog.error("entry of \(key, privacy: .public) names a metadata blob address that is not a content address")
                return nil
            }
            if !held {
                switch try await fetchHistory(entry, key: key) {
                case let .records(records): history = records
                case let .refused(verdict): return verdict
                }
            }
            metadata = try await verbs.fetchMetadataBlob(hash: address)
            syncLog.debug("entry of \(key, privacy: .public) metadata blob fetched: \(metadata.count, privacy: .public) bytes")
        }
        return try await open(
            Self.syncHead(entry, metadata: metadata, localChainHead: localChainHead),
            key: key,
            history: history,
            original: original
        )
    }

    /// Fetch `original` into the staging directory and run the chokepoint
    /// over it and `head`, removing the fetched ciphertext after: only
    /// verified when `head` carries no metadata blob, else opened, against
    /// `history` when it has records.
    private func open(
        _ head: FfiSyncHead,
        key: String,
        history: [Data],
        original: FfiBlobRef
    ) async throws -> SyncVerdict {
        let staged = try await fetchStaged(original)
        defer { removeStaged(staged) }
        guard !head.metadataBlob.isEmpty else {
            return SyncVerdict(outcome: try verbs.applySyncEntry(head, ciphertextAt: staged), plaintext: nil)
        }
        let plaintext = stagingDirectory.appending(path: UUID().uuidString)
        let opened: FfiOpenedOriginal
        do {
            if history.isEmpty {
                opened = try verbs.openSyncedOriginal(head, ciphertextAt: staged, to: plaintext)
            } else {
                opened = try verbs.openSyncedHistory(head, history: history, ciphertextAt: staged, to: plaintext)
                if case let .opened(facts) = opened {
                    syncLog.info(
                        "asset \(key, privacy: .public) opened from its \(history.count, privacy: .public)-record history, adopted=\(facts.adopted, privacy: .public)"
                    )
                }
            }
        } catch {
            removeStaged(plaintext)
            throw error
        }
        return SyncVerdict(opened, plaintext: plaintext, key: key)
    }

    /// The head the chokepoint runs `entry` as: its album and record, the
    /// metadata blob fetched for it (empty when it names none), and the head
    /// held for its asset.
    private static func syncHead(_ entry: FfiFeedEntry, metadata: Data, localChainHead: Data?) -> FfiSyncHead {
        FfiSyncHead(
            albumId: entry.albumId,
            manifestCbor: entry.manifestCbor,
            metadataBlob: metadata,
            localChainHead: localChainHead
        )
    }

    /// The records before `entry`'s, oldest first, for an asset this
    /// workspace does not hold: empty for a `create`. A history whose records
    /// do not decode, or that runs past the walk's bound (`FfiError.Sync`),
    /// never verifies, so it fails as a quarantined verdict; any other error,
    /// a record's fetch among them, is thrown, as a blob fetch's is.
    private func fetchHistory(_ entry: FfiFeedEntry, key: String) async throws -> HistoryFetch {
        do {
            let history = try await verbs.fetchProvenanceHistory(manifestCbor: entry.manifestCbor)
            syncLog.debug("entry of \(key, privacy: .public) has \(history.count, privacy: .public) earlier records")
            return .records(history)
        } catch let FfiError.Sync(_, detail) {
            syncLog.error("history of \(key, privacy: .public) is unreadable: \(detail, privacy: .public)")
            return .refused(SyncVerdict(
                outcome: .quarantined(reason: SyncVerdict.malformedHistoryReason, detail: detail),
                plaintext: nil
            ))
        }
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
}

// MARK: - Album-page tombstones

/// The two paths an album page's tombstone takes, which names no metadata
/// blob, no blob and no held original: adopted from its records when its
/// asset is not held here, applied against the held chain when it is.
extension FFISyncDriver {
    /// Adopt the asset of `entry`, which this workspace does not hold and
    /// which names no metadata blob (an album page's tombstone), from its
    /// records alone: fetch its history, name the live metadata blob and the
    /// original from it (``FFISyncDriverVerbs/historySources(manifestCbor:history:)``),
    /// fetch both, and open the entry with that metadata blob as one naming
    /// it is opened. The feed's `originalHeld` and blobs are not read: the
    /// tombstone lists neither, and the server serves both to the asset's
    /// readers until its retention window closes. A history whose records
    /// name no live metadata blob, or do not decode (`FfiError.Sync`), never
    /// adopts, so it is a quarantined verdict, logged, and nothing is fetched.
    ///
    /// `nil`, logged, for a tombstone there is nothing left to adopt from: its
    /// delete's window had closed at the feed's `changed_at`, judged before
    /// any fetch, or the server refuses one of its records or blobs for good
    /// (``FFISyncDriver/blobGoneCode``). The server never serves those again,
    /// so throwing would stall every sync at this page until an operator
    /// purges the asset; and passing the delete of an asset this device does
    /// not hold loses nothing. Any other fetch failure is thrown.
    private func adoptFromRecords(_ entry: FfiFeedEntry, key: String, localChainHead: Data?) async throws -> SyncVerdict? {
        syncLog.debug("entry of \(key, privacy: .public) names no metadata blob and its asset is not held: adopting from its records")
        return try await passingUnserved(entry, key: key) {
            try await adoptFetched(entry, key: key, localChainHead: localChainHead)
        }
    }

    /// Run `adopt` over `entry`, of an asset this workspace does not hold,
    /// unless nothing the server serves is left to adopt it from: `nil`,
    /// logged, for a tombstone whose delete's window had closed at the feed's
    /// `changed_at`, judged before `adopt` fetches anything, and for an entry
    /// one of whose records or blobs `adopt` is refused for good
    /// (``FFISyncDriver/blobGoneCode``). Any other failure is `adopt`'s own.
    private func passingUnserved(
        _ entry: FfiFeedEntry,
        key: String,
        adopt: () async throws -> SyncVerdict?
    ) async throws -> SyncVerdict? {
        if entry.kind == .deleted, windowClosed(entry, key: key) {
            syncLog.info(
                "entry of \(key, privacy: .public) is a tombstone whose retention window had closed at \(entry.changedAt, privacy: .public): passed, nothing fetched"
            )
            return nil
        }
        do {
            return try await adopt()
        } catch let FfiError.Fetch(code, detail) where code == Self.blobGoneCode {
            syncLog.info(
                "entry of \(key, privacy: .public) passed: the server no longer serves what adopting it needs: \(detail, privacy: .public)"
            )
            return nil
        }
    }

    /// Apply `entry`, an album page's tombstone of an asset this workspace
    /// holds, against the held chain and the original it commits to
    /// (``FFISyncDriverVerbs/applyHeldSyncEntry(_:)``), fetching nothing: the
    /// page names no blob, and the device already holds both. The delete binds
    /// no metadata blob, so the head carries none.
    private func applyHeldTombstone(_ entry: FfiFeedEntry, key: String, localChainHead: Data?) throws -> SyncVerdict {
        syncLog.debug("entry of \(key, privacy: .public) is a held asset's tombstone naming no original: applying it against the held chain")
        let head = Self.syncHead(entry, metadata: Data(), localChainHead: localChainHead)
        return try SyncVerdict(outcome: verbs.applyHeldSyncEntry(head), plaintext: nil)
    }

    /// Whether the window of `entry`'s delete had closed at its `changed_at`.
    /// A record the judgement cannot read is logged and read as open: its
    /// history fetch then decides, and quarantines a record that does not
    /// decode.
    private func windowClosed(_ entry: FfiFeedEntry, key: String) -> Bool {
        do {
            return try verbs.retentionClosed(manifestCbor: entry.manifestCbor, changedAt: entry.changedAt)
        } catch {
            syncLog.debug(
                "retention window of \(key, privacy: .public) unread, fetching its history: \(String(describing: error), privacy: .public)"
            )
            return false
        }
    }

    /// ``adoptFromRecords(_:key:localChainHead:)``'s fetches and open.
    private func adoptFetched(_ entry: FfiFeedEntry, key: String, localChainHead: Data?) async throws -> SyncVerdict {
        let history: [Data]
        switch try await fetchHistory(entry, key: key) {
        case let .records(records): history = records
        case let .refused(verdict): return verdict
        }
        let sources: FfiHistorySources
        do {
            sources = try verbs.historySources(manifestCbor: entry.manifestCbor, history: history)
        } catch let FfiError.Sync(_, detail) {
            syncLog.error(
                "entry of \(key, privacy: .public) refused: its \(history.count, privacy: .public)-record history names no live metadata blob: \(detail, privacy: .public)"
            )
            return SyncVerdict(
                outcome: .quarantined(reason: SyncVerdict.unboundHistoryReason, detail: detail),
                plaintext: nil
            )
        }
        syncLog.debug(
            "entry of \(key, privacy: .public) history names metadata blob \(sources.metadataBlobHash, privacy: .public) and original \(sources.original.ciphertextHash, privacy: .public) (\(sources.original.size, privacy: .public) bytes)"
        )
        let metadata = try await verbs.fetchMetadataBlob(hash: sources.metadataBlobHash)
        syncLog.debug("entry of \(key, privacy: .public) metadata blob fetched: \(metadata.count, privacy: .public) bytes")
        return try await open(
            Self.syncHead(entry, metadata: metadata, localChainHead: localChainHead),
            key: key,
            history: history,
            original: sources.original
        )
    }
}

// MARK: - Verdicts

/// The chokepoint's verdict on one entry, with the staged plaintext of its
/// original when the entry was opened.
struct SyncVerdict {
    /// The quarantine reason an opened entry whose file key could not be
    /// recovered, or whose ciphertext failed authentication, is passed with.
    static let undecryptableReason = "Undecryptable"
    /// The quarantine reason an opened entry whose plaintext does not hash to
    /// the signed sidecar's digest is passed with.
    static let digestMismatchReason = "DigestMismatch"
    /// The quarantine reason an entry of an unheld asset is passed with when
    /// its earlier records do not decode or run past the walk's bound, so its
    /// chain can never verify from its create.
    static let malformedHistoryReason = "MalformedHistory"
    /// The quarantine reason an entry of an unheld asset naming no metadata
    /// blob is passed with when its records name no live metadata blob, or
    /// one does not decode, so there is nothing to adopt it from.
    static let unboundHistoryReason = "UnboundHistory"

    let outcome: FfiSyncApplyOutcome
    /// The verified plaintext, present only for an `applied` outcome of an
    /// opened entry.
    let plaintext: URL?

    init(outcome: FfiSyncApplyOutcome, plaintext: URL?) {
        self.outcome = outcome
        self.plaintext = plaintext
    }

    /// The verdict `openSyncedOriginal` answered with, `plaintext` being the
    /// destination it was asked to open to. An original that applied but did
    /// not open writes nothing, so it is passed as quarantined, with the
    /// asset id as its detail.
    init(_ opened: FfiOpenedOriginal, plaintext: URL, key: String) {
        switch opened {
        case let .opened(facts):
            self.init(outcome: .applied(facts: facts), plaintext: plaintext)
        case let .refused(outcome):
            self.init(outcome: outcome, plaintext: nil)
        case let .undecryptable(facts):
            syncLog.error("original of \(key, privacy: .public) did not decrypt; nothing written")
            self.init(outcome: .quarantined(reason: Self.undecryptableReason, detail: facts.assetId), plaintext: nil)
        case let .digestMismatch(facts):
            syncLog.error("original of \(key, privacy: .public) does not match its signed digest; nothing written")
            self.init(outcome: .quarantined(reason: Self.digestMismatchReason, detail: facts.assetId), plaintext: nil)
        }
    }
}

/// Where judging one entry left it: awaiting an original the server does not
/// hold yet, which the feed lists again once it lands, or the chokepoint's
/// verdict, `nil` for an entry passed without one.
enum EntryJudgement {
    case awaitingOriginal
    case judged(SyncVerdict?)
}

/// What fetching an unheld asset's earlier records came to: the records,
/// oldest first, or the verdict an unreadable history passes its entry with.
enum HistoryFetch {
    case records([Data])
    case refused(SyncVerdict)
}
