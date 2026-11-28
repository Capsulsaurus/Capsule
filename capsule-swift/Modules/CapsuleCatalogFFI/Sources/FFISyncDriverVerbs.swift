import Foundation

// MARK: - Session seam

/// The session and workspace verbs a sync of the managed lane drives, bound
/// to one server: `FfiSession.syncPull`, `FfiSession.fetchBlobToPath`,
/// `FfiSession.fetchMetadataBlob`, `FfiSession.fetchProvenanceHistory`, `FfiWorkspace.applySyncEntryFromFile`,
/// `FfiWorkspace.applyHeldSyncEntry`, `FfiWorkspace.openSyncedOriginal`,
/// `FfiWorkspace.openSyncedHistory`, `FfiWorkspace.provenanceHead` and
/// `FfiWorkspace.assetIds`, and nothing else of either, plus the pure
/// `syncedHistorySources` and `syncedRetentionClosed`.
///
/// Narrowed to a protocol so the driver is tested over a scripted feed and
/// scripted verdicts rather than a live session.
protocol FFISyncDriverVerbs: Sendable {
    /// One page of the sync feed after `cursor` (empty for the feed's start).
    func syncPull(cursor: Data, pageSize: UInt32) async throws -> FfiSyncPage
    /// Fetch the blob at content address `hash` (`size` bytes) into the file
    /// at `path`, which holds exactly the verified ciphertext once this returns.
    func fetchBlob(hash: String, size: UInt64, to path: URL) async throws
    /// The metadata blob at content address `hash`, which the feed names
    /// without a size: read in bounded windows and verified against `hash`.
    func fetchMetadataBlob(hash: String) async throws -> Data
    /// The records before `manifestCbor` (a feed entry's latest record) back
    /// to its asset's `create`, oldest first; empty for a `create`. Nothing is
    /// verified: the walk only follows links.
    func fetchProvenanceHistory(manifestCbor: Data) async throws -> [Data]
    /// The live metadata blob and the original adopting the asset whose
    /// latest record is `manifestCbor` fetches, named from that record and
    /// `history` (what ``fetchProvenanceHistory(manifestCbor:)`` answered):
    /// what an album page's tombstone, which names neither, leaves out.
    /// Pure and offline; `FfiError.Sync` when a record does not decode or
    /// none binds a metadata blob.
    func historySources(manifestCbor: Data, history: [Data]) throws -> FfiHistorySources
    /// Whether the retention window of the `delete` that is `manifestCbor`
    /// had closed at `changedAt` (a feed entry's own, RFC 3339): `false` for
    /// a record that is not a `delete` or signs no readable window. Pure and
    /// offline; `FfiError.Sync` when the record does not decode.
    func retentionClosed(manifestCbor: Data, changedAt: String) throws -> Bool
    /// Run the sync chokepoint over `head` with the original's ciphertext read
    /// from the file at `path`. Writes nothing.
    func applySyncEntry(_ head: FfiSyncHead, ciphertextAt path: URL) throws -> FfiSyncApplyOutcome
    /// Run the sync chokepoint over `head`, of an asset the workspace holds,
    /// against the original its held chain commits to, fetching nothing, and
    /// adopt what applies: how a held asset's album-page tombstone, which
    /// names no blob, moves it to the trash.
    func applyHeldSyncEntry(_ head: FfiSyncHead) throws -> FfiSyncApplyOutcome
    /// Run the sync chokepoint over `head` with the original's ciphertext read
    /// from the file at `path`, and decrypt it to `destination`, which holds
    /// the verified plaintext only when this answers `opened`.
    func openSyncedOriginal(_ head: FfiSyncHead, ciphertextAt path: URL, to destination: URL) throws -> FfiOpenedOriginal
    /// ``openSyncedOriginal(_:ciphertextAt:to:)`` for an asset the workspace
    /// does not hold, first seen past its create: `head` is applied against
    /// the last record of `history` (what ``fetchProvenanceHistory(manifestCbor:)``
    /// answered), every record is verified from the create, and the asset is
    /// adopted, which the opened facts' `adopted` says.
    func openSyncedHistory(
        _ head: FfiSyncHead,
        history: [Data],
        ciphertextAt path: URL,
        to destination: URL
    ) throws -> FfiOpenedOriginal
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

    func fetchMetadataBlob(hash: String) async throws -> Data {
        try await session.fetchMetadataBlob(
            blobBaseUrl: Self.blobBaseURL(apiBaseURL: apiBaseURL),
            hash: hash
        )
    }

    func fetchProvenanceHistory(manifestCbor: Data) async throws -> [Data] {
        try await session.fetchProvenanceHistory(
            blobBaseUrl: Self.blobBaseURL(apiBaseURL: apiBaseURL),
            manifestCbor: manifestCbor
        )
    }

    func historySources(manifestCbor: Data, history: [Data]) throws -> FfiHistorySources {
        try syncedHistorySources(manifestCbor: manifestCbor, history: history)
    }

    func retentionClosed(manifestCbor: Data, changedAt: String) throws -> Bool {
        try syncedRetentionClosed(manifestCbor: manifestCbor, changedAt: changedAt)
    }

    func applySyncEntry(_ head: FfiSyncHead, ciphertextAt path: URL) throws -> FfiSyncApplyOutcome {
        try workspace.applySyncEntryFromFile(entry: head, ciphertextPath: path.path)
    }

    func applyHeldSyncEntry(_ head: FfiSyncHead) throws -> FfiSyncApplyOutcome {
        try workspace.applyHeldSyncEntry(entry: head)
    }

    func openSyncedOriginal(_ head: FfiSyncHead, ciphertextAt path: URL, to destination: URL) throws -> FfiOpenedOriginal {
        try workspace.openSyncedOriginal(entry: head, ciphertextPath: path.path, destinationPath: destination.path)
    }

    func openSyncedHistory(
        _ head: FfiSyncHead,
        history: [Data],
        ciphertextAt path: URL,
        to destination: URL
    ) throws -> FfiOpenedOriginal {
        try workspace.openSyncedHistory(
            entry: head,
            history: history,
            ciphertextPath: path.path,
            destinationPath: destination.path
        )
    }

    func provenanceHead(assetId: String) throws -> Data {
        try workspace.provenanceHead(assetId: assetId)
    }

    func assetIds() throws -> [String] {
        try workspace.assetIds()
    }
}
