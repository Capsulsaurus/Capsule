import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - MockTransferStore

/// Everything that moves bytes: imports in, uploads out, reconciliation both
/// ways, local disk accounting, and the server-side quota position.
///
/// One actor because these five contradict each other if they drift. An upload
/// cannot be in flight while the connection class says offline; a local copy
/// cannot be released while its asset is not durable; a new session cannot open
/// while quota is hard-exceeded. Each of those is a rule some screen will lean
/// on, and a mock that let two of them disagree would make the screen look
/// broken when it was right.
public actor MockTransferStore {
    nonisolated let store: MockLibraryStore
    nonisolated let configuration: MockConfiguration
    private nonisolated let gate: MockGate

    private var sessions: [UploadSession]
    private var policy: UploadPolicy
    private var scope: SyncScope
    private var status: SyncStatus
    private var quotaStatus: QuotaStatus
    private var pinnedAssets: Set<AssetID> = []
    private var evictedBytes: UInt64 = 0
    private var cachedBreakdown: LocalStorageBreakdown?
    private var cancelledImports: Set<ImportID> = []

    nonisolated let uploadChanges = ChangeBroadcaster<[UploadSession]>()
    nonisolated let syncChanges = ChangeBroadcaster<SyncStatus>()
    nonisolated let quotaChanges = ChangeBroadcaster<QuotaStatus>()

    public init(store: MockLibraryStore, configuration: MockConfiguration) {
        self.store = store
        self.configuration = configuration
        gate = MockGate(behaviour: configuration.behaviour)
        policy = configuration.uploadPolicy
        scope = configuration.syncScope
        quotaStatus = configuration.quota
        sessions = Self.seedSessions(configuration: configuration)
        status = Self.seedStatus(configuration: configuration)
    }

    /// The sessions a resuming client would rebuild from server truth.
    ///
    /// Under a **staged** policy they sit in tier order — index first, then
    /// preview, then original — because that is all `staged` means: the client
    /// has not opened the higher-tier session yet. There is no second code path
    /// and no server mode branch, and a UI implying otherwise is lying about the
    /// protocol.
    private static func seedSessions(configuration: MockConfiguration) -> [UploadSession] {
        let library = MockLibrary(profile: configuration.profile)
        guard library.assetCount > 0 else { return [] }
        let count = configuration.stallsUploads ? 9 : 4
        return (0 ..< count).compactMap { ordinal -> UploadSession? in
            let index = ordinal * 7
            guard index < library.assetCount else { return nil }
            let ref = MockAssetRef(kind: .live, index: index)
            let total = library.byteSize(for: ref, contentType: library.contentType(for: ref))
            let tier = UploadTier.ladder[ordinal % UploadTier.ladder.count]
            return UploadSession(
                id: MockIdentifiers.uploadID(seed: configuration.seed, ordinal: ordinal),
                assetID: ref.uuidString(seed: configuration.seed),
                blobRole: tier == .original ? .original : .derivative,
                tier: tier,
                state: configuration.stallsUploads ? .pending : .uploading,
                offset: configuration.stallsUploads ? 0 : total / 3,
                declaredSize: max(1, total),
                ciphertextHash: MockIdentifiers.blobHash(seed: configuration.seed, ordinal: index)
            )
        }
    }

    private static func seedStatus(configuration: MockConfiguration) -> SyncStatus {
        let isStale = configuration.stallsUploads || configuration.connectionClass == .offline
        return SyncStatus(
            lastCompletedSyncAt: configuration.clock.offset(days: isStale ? -23 : -1),
            pendingUploadCount: configuration.stallsUploads ? 812 : 3,
            pendingDownloadCount: configuration.connectionClass == .offline ? 47 : 0,
            connectionClass: configuration.connectionClass,
            isSyncing: false
        )
    }

    // MARK: State

    var currentSessions: [UploadSession] { sessions }
    var currentPolicy: UploadPolicy { policy }
    var currentScope: SyncScope { scope }
    var currentStatus: SyncStatus { status }
    var currentQuota: QuotaStatus { quotaStatus }
    var pinned: Set<AssetID> { pinnedAssets }
    var reclaimedBytes: UInt64 { evictedBytes }
    var behaviourGate: MockGate { gate }

    func setSessions(_ value: [UploadSession]) { sessions = value }
    func setPolicy(_ value: UploadPolicy) { policy = value }
    func setScope(_ value: SyncScope) { scope = value }
    func setStatus(_ value: SyncStatus) { status = value }
    func setQuota(_ value: QuotaStatus) { quotaStatus = value }
    func updatePinned(_ value: Set<AssetID>) { pinnedAssets = value }
    func addReclaimed(_ value: UInt64) { evictedBytes += value
        cachedBreakdown = nil
    }

    func markImportCancelled(_ identifier: ImportID) { cancelledImports.insert(identifier) }
    func isImportCancelled(_ identifier: ImportID) -> Bool { cancelledImports.contains(identifier) }

    func breakdown(_ make: () -> LocalStorageBreakdown) -> LocalStorageBreakdown {
        if let cachedBreakdown { return cachedBreakdown }
        let value = make()
        cachedBreakdown = value
        return value
    }
}
