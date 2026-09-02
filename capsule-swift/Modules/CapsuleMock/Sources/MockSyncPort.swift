import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

// MARK: - SyncPort

extension MockTransferStore: SyncPort {
    public func status() async throws -> SyncStatus {
        currentStatus
    }

    /// Run a reconciliation now, subject to the connection criteria.
    ///
    /// Gated, so ``MockScenario/offline`` and
    /// ``MockScenario/protocolUpgradeRequired`` refuse here for real. This is
    /// the boundary local reads deliberately do not cross.
    public func synchronize() async throws {
        guard currentStatus.connectionClass.isUsable else {
            throw CapsuleError(code: .syncCursorInvalid, detail: "CapsuleMock: no usable connection")
        }
        try await behaviourGate.admit()
        await completeSync()
    }

    /// Reconcile **regardless** of the metered and Wi-Fi criteria.
    ///
    /// The one-tap escape hatch offered with the staleness notification, and
    /// never automatic — it spends the user's data on their explicit say-so, so
    /// it may only ever be something they chose.
    public func forceSynchronize() async throws {
        try await behaviourGate.admit()
        await completeSync()
    }

    /// Snooze the staleness notification.
    ///
    /// Suppresses the **warning** only. Auto sync is unaffected — a user who
    /// dismisses a notice has not asked to stop syncing, and conflating the two
    /// would quietly strand their library.
    public func snoozeStalenessNotification(until: CapsuleTimestamp) async throws {
        var status = currentStatus
        status.staleNotificationSnoozedUntil = until
        setStatus(status)
        await syncChanges.send(status)
    }

    public func syncScope() async throws -> SyncScope {
        currentScope
    }

    public func setSyncScope(_ scope: SyncScope) async throws {
        try scope.requireWritable()
        setScope(scope)
    }

    /// Fetch a representation on demand.
    ///
    /// On a permanent failure it **degrades** to the best representation in hand
    /// and reports ``AssetSyncState/fullResolutionUnavailable(bestAvailable:)``.
    /// It never removes the asset's metadata or index entry over a missing
    /// derivative — a missing thumbnail is not a missing photograph, and a
    /// client that treated it as one would manufacture data loss.
    public func fetchRepresentation(
        _ tier: RepresentationTier,
        for assetID: AssetID
    ) async throws -> LocalRepresentations {
        guard let asset = await store.engine.asset(for: assetID) else {
            throw CapsuleError(code: .blobPendingUpload, detail: "CapsuleMock: unknown asset")
        }
        guard currentStatus.connectionClass.isUsable else {
            let degraded = asset.representations
            await store.applyFetchOutcome(
                assetID,
                representations: degraded,
                state: .fullResolutionUnavailable(bestAvailable: degraded.best)
            )
            return degraded
        }
        let fetched = asset.representations.adding(tier)
        await store.applyFetchOutcome(assetID, representations: fetched, state: .durable)
        return fetched
    }

    public nonisolated func changes() -> AsyncStream<SyncStatus> {
        syncChanges.subscribe()
    }

    private func completeSync() async {
        var status = currentStatus
        status.lastCompletedSyncAt = configuration.clock.now
        status.pendingUploadCount = 0
        status.pendingDownloadCount = 0
        status.isSyncing = false
        setStatus(status)
        await syncChanges.send(status)
    }
}
