import Foundation

// MARK: - Session seam

/// The session and workspace verbs a purge or lifecycle delivery drives,
/// bound to one server: `FfiSession.syncPull`,
/// `FfiSession.deliverPurgeAtHash`, `FfiSession.deliverLifecycleAtHash`,
/// `FfiWorkspace.purgedRecord`, `FfiWorkspace.purgedAssetIds`,
/// `FfiWorkspace.provenanceHead` and `FfiWorkspace.assetIds`, and nothing
/// else of either.
///
/// Narrowed to a protocol so the delivery is tested over a scripted feed and
/// outcomes rather than a live session.
protocol FFIPurgeDeliveryVerbs: Sendable {
    /// One page of the sync feed after `cursor` (empty for the feed's start).
    func syncPull(cursor: Data, pageSize: UInt32) async throws -> FfiSyncPage
    /// Send what the server lacks of the purged asset's kept chain, given the
    /// record hash of the head the feed lists (empty for an entry that
    /// carries none, `nil` for an asset it does not list).
    func deliverPurge(assetId: String, serverHeadHash: Data?) async throws -> FfiLifecycleOpOutcome
    /// The head of the purged asset's kept chain, as canonical CBOR; `nil`
    /// for an asset the workspace has not purged.
    func purgedRecord(assetId: String) throws -> Data?
    /// Every asset the workspace has purged.
    func purgedAssetIds() throws -> [String]
    /// Send what the server lacks of a held asset's chain, given the record
    /// hash of the head the feed lists (as ``deliverPurge(assetId:serverHeadHash:)``)
    /// and the latest `changed_at` the drain saw (RFC 3339, `nil` for none),
    /// which floors the clock the held rules judge by.
    func deliverLifecycle(
        assetId: String,
        serverHeadHash: Data?,
        latestChangedAt: String?
    ) async throws -> FfiLifecycleOpOutcome
    /// The head of a held asset's chain, as canonical CBOR.
    func provenanceHead(assetId: String) throws -> Data
    /// Every asset the workspace holds, live or in the trash.
    func assetIds() throws -> [String]
}

/// The verbs over a signed-in session and the workspace it delivers from.
struct FFISessionPurgeVerbs: FFIPurgeDeliveryVerbs {
    let session: FfiSession
    let workspace: FfiWorkspace
    let apiBaseURL: String

    func syncPull(cursor: Data, pageSize: UInt32) async throws -> FfiSyncPage {
        try await session.syncPull(apiBaseUrl: apiBaseURL, cursor: cursor, pageSize: pageSize)
    }

    func deliverPurge(assetId: String, serverHeadHash: Data?) async throws -> FfiLifecycleOpOutcome {
        try await session.deliverPurgeAtHash(
            apiBaseUrl: apiBaseURL,
            workspace: workspace,
            assetId: assetId,
            serverHeadHash: serverHeadHash
        )
    }

    func purgedRecord(assetId: String) throws -> Data? {
        try workspace.purgedRecord(assetId: assetId)
    }

    func purgedAssetIds() throws -> [String] {
        try workspace.purgedAssetIds()
    }

    func deliverLifecycle(
        assetId: String,
        serverHeadHash: Data?,
        latestChangedAt: String?
    ) async throws -> FfiLifecycleOpOutcome {
        try await session.deliverLifecycleAtHash(
            apiBaseUrl: apiBaseURL,
            workspace: workspace,
            assetId: assetId,
            serverHeadHash: serverHeadHash,
            latestChangedAt: latestChangedAt
        )
    }

    func provenanceHead(assetId: String) throws -> Data {
        try workspace.provenanceHead(assetId: assetId)
    }

    func assetIds() throws -> [String] {
        try workspace.assetIds()
    }
}
