import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

@testable import FeatureImport

// MARK: - StubImportPort

/// A programmable ``ImportPort``.
///
/// The suites in this module test *view models*, and a view model's job is to
/// fold port answers into screen state. Driving that with `CapsuleMock` would
/// test the mock's derivation as much as the model, and would make an assertion
/// about "four conflicts" depend on a hash. Every answer here is set at
/// construction, so each test states its own world in three lines.
actor StubImportPort: ImportPort {
    private let scopes: [ImportScope]
    private let scanResult: ImportScan
    private let plannedResult: ImportPlan
    private let sessions: [ImportSessionRecord]
    private let retryResults: [ImportResult]
    private let planError: (any Error)?
    private let events: [ImportProgressEvent]

    /// Locators the last ``retry(_:locators:)`` was asked for.
    private(set) var retriedLocators: [String] = []
    /// Sessions the screen asked to forget.
    private(set) var dismissedSessions: [ImportID] = []
    /// Whether ``cancel(_:)`` was called.
    private(set) var didCancel = false

    init(
        scopes: [ImportScope] = [],
        scan: ImportScan = ImportScan(scope: StubFixtures.cameraRollScope, candidates: []),
        plan: ImportPlan = StubFixtures.plan(),
        sessions: [ImportSessionRecord] = [],
        retryResults: [ImportResult] = [],
        planError: (any Error)? = nil,
        events: [ImportProgressEvent] = []
    ) {
        self.scopes = scopes
        scanResult = scan
        plannedResult = plan
        self.sessions = sessions
        self.retryResults = retryResults
        self.planError = planError
        self.events = events
    }

    func availableScopes() async throws -> [ImportScope] { scopes }

    func scan(_: ImportScope) async throws -> ImportScan { scanResult }

    func plan(
        _: ImportScan,
        destination _: AlbumID?,
        mode _: ImportMode,
        uploadPolicy _: UploadPolicy,
        streaming _: Bool
    ) async throws -> ImportPlan {
        if let planError { throw planError }
        return plannedResult
    }

    nonisolated func execute(_: ImportPlan) -> AsyncStream<ImportProgressEvent> {
        AsyncStream { continuation in
            for event in events {
                continuation.yield(event)
            }
            continuation.finish()
        }
    }

    func cancel(_: ImportID) async throws { didCancel = true }

    func resolveScope(sourceKind: SourceKind, locator: String) async throws -> ImportScope {
        ImportScope(
            scopeID: "resolved-\(sourceKind.rawValue)",
            platform: PlatformTag(rawValue: "ios"),
            sourceKind: sourceKind,
            locator: locator
        )
    }

    nonisolated func scanStream(_: ImportScope) -> AsyncStream<ImportScanEvent> {
        AsyncStream { continuation in
            continuation.yield(.started(expectedTotal: scanResult.candidates.count))
            continuation.yield(.finished(scanResult))
            continuation.finish()
        }
    }

    func retry(_: ImportID, locators: [String]) async throws -> [ImportResult] {
        retriedLocators.append(contentsOf: locators)
        return retryResults.filter { locators.contains($0.locator) }
    }

    func history(limit: Int) async throws -> [ImportSessionRecord] {
        Array(sessions.prefix(limit))
    }

    func replan(_: ImportID) async throws -> ImportPlan { plannedResult }

    func dismissSession(_ importID: ImportID) async throws {
        dismissedSessions.append(importID)
    }
}

// MARK: - StubStoragePort

/// A ``StoragePort`` that reports exactly the free space a test asks for.
struct StubStoragePort: StoragePort {
    var availableDiskBytes: UInt64?

    func localBreakdown() async throws -> LocalStorageBreakdown {
        LocalStorageBreakdown(availableDiskBytes: availableDiskBytes)
    }

    func verify(assetIDs: [AssetID], deep _: Bool) async throws -> [StorageVerification] {
        assetIDs.map { _ in
            StorageVerification(assetID: "", durable: true, blobs: [], checkedAt: StubFixtures.now)
        }
    }

    func releaseLocalCopies(for _: [AssetID]) async throws {}

    func evictCache(targetBytes: UInt64) async throws -> UInt64 { targetBytes }

    func setPinned(_: Bool, for _: [AssetID]) async throws {}
}

// MARK: - StubSyncPort

/// A ``SyncPort`` that reports one fixed connection class.
///
/// Present only so ``ImportConnectivity`` can answer; nothing in these suites
/// reconciles anything.
struct StubSyncPort: SyncPort {
    var connectionClass: ConnectionClass = .unmetered

    func status() async throws -> SyncStatus {
        SyncStatus(
            lastCompletedSyncAt: StubFixtures.now,
            pendingUploadCount: 0,
            pendingDownloadCount: 0,
            connectionClass: connectionClass,
            isSyncing: false
        )
    }

    func synchronize() async throws {}
    func forceSynchronize() async throws {}
    func snoozeStalenessNotification(until _: CapsuleTimestamp) async throws {}
    func syncScope() async throws -> SyncScope { .metadataAndThumbnails }
    func setSyncScope(_: SyncScope) async throws {}

    func fetchRepresentation(_: RepresentationTier, for _: AssetID) async throws -> LocalRepresentations {
        LocalRepresentations()
    }

    func changes() -> AsyncStream<SyncStatus> {
        AsyncStream { $0.finish() }
    }
}

// MARK: - StubFixtures

/// Fixed values the suites build their worlds from.
enum StubFixtures {
    /// 2026-08-22T12:00:00Z — the same instant `CapsuleMock` anchors on, so a
    /// stubbed world and a mock world agree about what time it is.
    static let nowEpochSeconds: Int64 = 1787400000
    static let now = CapsuleTimestamp(epochSeconds: nowEpochSeconds)
    static let clock = ImportClock.fixed(epochSeconds: nowEpochSeconds)

    static let cameraRollScope = ImportScope(
        scopeID: "scope-camera-roll",
        platform: PlatformTag(rawValue: "ios"),
        sourceKind: .cameraRoll,
        locator: "photokit://camera-roll"
    )

    static let volumeScope = ImportScope(
        scopeID: "scope-volume",
        platform: PlatformTag(rawValue: "macos"),
        sourceKind: .removableVolume,
        locator: "file:///Volumes/SD-CARD"
    )

    static func candidate(_ ordinal: Int, byteSize: UInt64 = 1000000) -> ImportCandidate {
        ImportCandidate(
            id: "candidate-\(ordinal)",
            locator: "photokit://camera-roll/IMG_\(ordinal).HEIC",
            contentType: .heic,
            byteSize: byteSize
        )
    }

    static func scan(candidateCount: Int, byteSize: UInt64 = 1000000) -> ImportScan {
        ImportScan(
            scope: cameraRollScope,
            candidates: (0 ..< candidateCount).map { candidate($0, byteSize: byteSize) }
        )
    }

    static func plan(
        importing: Int = 3,
        skipping: Int = 1,
        conflicts: [ImportConflict] = [],
        byteSize: UInt64 = 1000000,
        rule: ImportPlan.DestinationRule = .scopeOverride,
        mode: ImportMode = .copy,
        streaming: Bool = false,
        uploadPolicy: UploadPolicy = .full
    ) -> ImportPlan {
        let imported = (0 ..< importing).map { ordinal in
            ImportDecision(candidate: candidate(ordinal, byteSize: byteSize), action: .importAsset)
        }
        let skipped = (0 ..< skipping).map { ordinal in
            ImportDecision(
                candidate: candidate(importing + ordinal, byteSize: byteSize),
                action: .skipDuplicate(existingAssetID: "existing-\(ordinal)")
            )
        }
        return ImportPlan(
            id: ImportID("stub-import"),
            scope: cameraRollScope,
            destinationAlbumID: AlbumID.managed(uuid: "stub-album"),
            destinationRule: rule,
            mode: mode,
            uploadPolicy: uploadPolicy,
            isStreaming: streaming,
            decisions: imported + skipped,
            conflicts: conflicts
        )
    }

    static func conflict(
        _ ordinal: Int,
        kind: ImportConflictKind = .sameNameDifferentContent,
        resolution: ImportConflictResolution? = nil
    ) -> ImportConflict {
        ImportConflict(
            candidateID: "candidate-\(ordinal)",
            locator: "photokit://camera-roll/IMG_\(ordinal).HEIC",
            kind: kind,
            existingAssetID: "existing-\(ordinal)",
            resolution: resolution
        )
    }

    static func session(
        _ ordinal: Int,
        failures: Int = 0,
        imported: Int = 4,
        cancelled: Bool = false
    ) -> ImportSessionRecord {
        let results = (0 ..< imported).map { position in
            ImportResult(
                locator: "photokit://camera-roll/IMG_\(position).HEIC",
                outcome: .imported(assetID: "asset-\(position)", derivativesDeferred: false)
            )
        } + (0 ..< failures).map { position in
            ImportResult(
                locator: "photokit://camera-roll/FAIL_\(position).HEIC",
                outcome: .failed(.uploadChecksumMismatch)
            )
        }
        return ImportSessionRecord(
            id: ImportID("session-\(ordinal)"),
            scope: cameraRollScope,
            destinationAlbumID: AlbumID.managed(uuid: "stub-album"),
            destinationRule: .sourceKindDefault,
            mode: .copy,
            startedAt: CapsuleTimestamp(epochSeconds: nowEpochSeconds - Int64(ordinal) * 86400 - 600),
            finishedAt: CapsuleTimestamp(epochSeconds: nowEpochSeconds - Int64(ordinal) * 86400 - 300),
            summary: ImportSummary(id: ImportID("session-\(ordinal)"), results: results),
            wasCancelled: cancelled
        )
    }

    /// The connectivity probe every suite uses, on a healthy link.
    static let connectivity = ImportConnectivity(sync: StubSyncPort())
}
