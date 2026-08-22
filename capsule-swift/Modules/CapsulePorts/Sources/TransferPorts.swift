import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - ImportPort

/// Bringing external files into the library: **scan → plan → execute**.
///
/// The three stages are separate calls, not one `import(from:)`, because the
/// middle one is a decision point. A plan names the destination, the mode, and
/// every per-candidate outcome *before* anything is written — which is the only
/// way a user can meaningfully consent to ``ImportMode/move``, an operation that
/// deletes their source files.
public protocol ImportPort: Sendable {
    /// The import sources this device can see.
    ///
    /// Maps to `import.list_scopes`.
    func availableScopes() async throws -> [ImportScope]

    /// Enumerate a source. Reads nothing into the library.
    ///
    /// Maps to `import.scan`.
    func scan(_ scope: ImportScope) async throws -> ImportScan

    /// Turn a scan into a plan: resolve the destination, detect duplicates and
    /// companions, and decide each candidate's fate.
    ///
    /// - Throws: when the configuration is invalid — notably a
    ///   ``UploadPolicy/staged`` policy combined with streaming, which the
    ///   planner **rejects outright** rather than silently choosing one.
    ///
    /// Maps to `import.plan`.
    func plan(
        _ scan: ImportScan,
        destination: AlbumID?,
        mode: ImportMode,
        uploadPolicy: UploadPolicy,
        streaming: Bool
    ) async throws -> ImportPlan

    /// Execute a confirmed plan, streaming progress.
    ///
    /// The stream is the whole interface to a running import: it ends with
    /// ``ImportProgressEvent/finished(summary:)`` or
    /// ``ImportProgressEvent/cancelled(summary:)``, and cancelling the task
    /// that consumes it cancels the run. Cancellation is a **stop, not a
    /// rollback** — everything already imported stays imported.
    ///
    /// Maps to `import.execute`.
    func execute(_ plan: ImportPlan) -> AsyncStream<ImportProgressEvent>

    /// Cancel a run started by ``execute(_:)``.
    ///
    /// Maps to `import.cancel`.
    func cancel(_ importID: ImportID) async throws
}

// MARK: - UploadPort

/// Outbound transfers and their receipts.
///
/// The port surfaces sessions rather than bytes: chunking, alignment, offset
/// arithmetic, and the adaptive chunk-size ladder all live in the SDK, where
/// exactly one implementation can get them right. What the UI needs is which
/// blobs are in flight, how far along, and whether custody is proven.
public protocol UploadPort: Sendable {
    /// This device's active sessions, re-derived from server truth on resume —
    /// the local work queue is a rebuildable cache, never the source of truth.
    ///
    /// Maps to `upload.list_sessions`.
    func activeSessions() async throws -> [UploadSession]

    /// The device's upload policy.
    ///
    /// Maps to `settings.get_upload_policy`.
    func uploadPolicy() async throws -> UploadPolicy

    /// Set the upload policy. Client-side session **ordering** only: the server
    /// has no mode branch to switch.
    ///
    /// Maps to `settings.set_upload_policy`.
    func setUploadPolicy(_ policy: UploadPolicy) async throws

    /// Force a transfer regardless of the metered and Wi-Fi criteria, on the
    /// user's explicit consent.
    ///
    /// Maps to `upload.force_sync`.
    func forceUpload(assetIDs: [AssetID]) async throws

    /// Cancel a session. Refused once finalization has begun — finalization is
    /// not interruptible.
    ///
    /// Maps to `upload.cancel_session`.
    func cancelSession(_ id: UploadID) async throws

    /// The custody receipts held for an asset — the evidence half of
    /// verify-before-destroy.
    ///
    /// Maps to `upload.receipts_for_asset`.
    func custodyReceipts(for assetID: AssetID) async throws -> [CustodyReceipt]

    /// A stream of session-state changes.
    func changes() -> AsyncStream<[UploadSession]>
}

// MARK: - SyncPort

/// Reconciliation with the home server, in both directions.
public protocol SyncPort: Sendable {
    /// The library's current standing — pending work, connection class, and
    /// whether it has gone stale.
    ///
    /// Maps to `sync.status`.
    func status() async throws -> SyncStatus

    /// Run a reconciliation now, subject to the connection criteria.
    ///
    /// Maps to `sync.reconcile`.
    func synchronize() async throws

    /// Run a reconciliation **regardless** of the metered and Wi-Fi criteria —
    /// the one-tap escape hatch offered with the two-week staleness
    /// notification, and never automatic.
    ///
    /// Maps to `sync.force`.
    func forceSynchronize() async throws

    /// Snooze the staleness notification. Suppresses the **warning** only; auto
    /// sync itself is unaffected.
    ///
    /// Maps to `sync.snooze_staleness`.
    func snoozeStalenessNotification(until: CapsuleTimestamp) async throws

    /// The per-library fetch scope.
    ///
    /// Maps to `settings.get_sync_scope`.
    func syncScope() async throws -> SyncScope

    /// Set the fetch scope.
    ///
    /// Maps to `settings.set_sync_scope`.
    func setSyncScope(_ scope: SyncScope) async throws

    /// Fetch a specific representation on demand — a preview when an asset is
    /// opened, an original when it is exported.
    ///
    /// On a permanent failure the implementation **degrades** to the best
    /// representation in hand and reports
    /// ``AssetSyncState/fullResolutionUnavailable(bestAvailable:)``; it never
    /// removes the asset's metadata or index entry over a missing derivative.
    ///
    /// Maps to `sync.fetch_representation`.
    func fetchRepresentation(
        _ tier: RepresentationTier,
        for assetID: AssetID
    ) async throws -> LocalRepresentations

    /// A stream of sync-status updates.
    func changes() -> AsyncStream<SyncStatus>
}

// MARK: - StoragePort

/// Local disk accounting and the verify-before-destroy gate.
///
/// The gate is the reason this port is separate from ``QuotaPort``: quota is
/// about what the *server* is charging, storage is about what this *device*
/// holds and whether it is safe to stop holding it. Conflating them is how a
/// cache-clearing feature ends up deleting an only copy.
public protocol StoragePort: Sendable {
    /// What this device is spending disk on, by tier, with the trash segment
    /// broken out.
    ///
    /// Maps to `storage.local_breakdown`.
    func localBreakdown() async throws -> LocalStorageBreakdown

    /// Ask the server whether these assets are stored, indexed, and retrievable
    /// **right now**.
    ///
    /// A point-in-time fact, not a standing guarantee — see
    /// ``StorageVerification/authorisesRelease(at:)`` for the freshness rule the
    /// caller must apply before acting on it.
    ///
    /// Maps to `storage.verify`.
    func verify(assetIDs: [AssetID], deep: Bool) async throws -> [StorageVerification]

    /// Release local bytes for assets confirmed durable.
    ///
    /// - Throws: for any asset that is not durable. A non-durable verdict
    ///   **never** triggers a destructive action: the client keeps the copy,
    ///   retries with backoff, and surfaces "not yet confirmed on server".
    ///
    /// Maps to `storage.release_local`.
    func releaseLocalCopies(for assetIDs: [AssetID]) async throws

    /// Evict re-fetchable cached tiers to reclaim space. Never touches a
    /// device-owned original that has not been confirmed durable.
    ///
    /// Maps to `storage.evict_cache`.
    func evictCache(targetBytes: UInt64) async throws -> UInt64

    /// Pin an asset so it is exempt from cache eviction — offline access.
    ///
    /// Maps to `storage.set_pinned`.
    func setPinned(_ pinned: Bool, for assetIDs: [AssetID]) async throws
}

// MARK: - QuotaPort

/// The account's server-side storage position.
public protocol QuotaPort: Sendable {
    /// Current usage, limits, and state.
    ///
    /// Maps to `quota.status`.
    func status() async throws -> QuotaStatus

    /// Whether a prospective upload of this size would be admitted, checked
    /// before starting one rather than discovering it mid-import.
    ///
    /// Maps to `quota.check`.
    func wouldAdmit(additionalBytes: UInt64) async throws -> Bool

    /// A stream of quota changes, so a warning banner appears the moment a
    /// threshold is crossed rather than on the next screen visit.
    func changes() -> AsyncStream<QuotaStatus>
}
