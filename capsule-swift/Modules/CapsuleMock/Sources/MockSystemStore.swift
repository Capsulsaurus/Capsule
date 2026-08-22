import CapsuleDomain
import CapsuleFoundation
import Foundation

// MARK: - MockSystemStore

/// Quarantine, maintenance, and the local settings document.
///
/// Together because they are the "what is this device doing about the library"
/// surfaces, and because they share a gate: a destructive maintenance job runs
/// behind verify-before-destroy, and a quarantined item is what a job produces
/// when it will not proceed. Keeping them in one actor means a job that
/// quarantines something can announce both facts.
public actor MockSystemStore {
    nonisolated let configuration: MockConfiguration

    private var items: [QuarantineItem]
    private var taskStates: [MaintenanceTaskKind: MaintenanceTask]
    private var taskOrder: [MaintenanceTaskKind]
    private var localSettings: LibrarySettings
    private var defaultAlbum: AlbumID
    private var overrides: [ImportScope: AlbumID] = [:]
    private var cancelledTasks: Set<MaintenanceTaskKind> = []

    nonisolated let quarantineChanges = ChangeBroadcaster<Void>()
    nonisolated let maintenanceChanges = ChangeBroadcaster<[MaintenanceTask]>()
    nonisolated let settingsChanges = ChangeBroadcaster<LibrarySettings>()

    public init(configuration: MockConfiguration) {
        self.configuration = configuration
        items = MockQuarantineSeed.items(configuration: configuration)
        let seeded = MockMaintenanceSeed.tasks(configuration: configuration)
        taskOrder = seeded.map(\.kind)
        taskStates = Dictionary(uniqueKeysWithValues: seeded.map { ($0.kind, $0) })
        localSettings = LibrarySettings(
            syncScope: configuration.syncScope,
            uploadPolicy: configuration.uploadPolicy,
            autoSyncEnabled: configuration.connectionClass != .offline,
            cacheBudgetBytes: 24 * 1_073_741_824,
            aiProcessingEnabled: true,
            aiRequiresPower: true,
            stalenessNotificationEnabled: true
        )
        defaultAlbum = MockIdentifiers.albumID(seed: configuration.seed, ordinal: 0)
    }

    // MARK: State

    var quarantineItems: [QuarantineItem] { items }
    var taskList: [MaintenanceTask] { taskOrder.compactMap { taskStates[$0] } }
    var currentSettings: LibrarySettings { localSettings }
    var currentDefaultAlbum: AlbumID { defaultAlbum }
    var currentOverrides: [ImportScope: AlbumID] { overrides }

    func setItems(_ value: [QuarantineItem]) { items = value }
    func setTask(_ task: MaintenanceTask) { taskStates[task.kind] = task }
    func setSettings(_ value: LibrarySettings) { localSettings = value }
    func setDefaultAlbum(_ value: AlbumID) { defaultAlbum = value }
    func setOverride(_ value: AlbumID?, for scope: ImportScope) { overrides[scope] = value }
    func markCancelled(_ kind: MaintenanceTaskKind) { cancelledTasks.insert(kind) }
    func clearCancelled(_ kind: MaintenanceTaskKind) { cancelledTasks.remove(kind) }
    func isCancelled(_ kind: MaintenanceTaskKind) -> Bool { cancelledTasks.contains(kind) }
}

// MARK: - MockQuarantineSeed

/// The quarantine inventory a scenario starts with.
enum MockQuarantineSeed {
    /// One entry per configured surface.
    ///
    /// ``MockScenario/quarantine`` configures **several distinct surfaces**
    /// rather than several rows of one, because the surface is what decides
    /// where the bytes are and therefore what the user can do. A screen tested
    /// against six rows that all say "verify rejected" has never rendered the
    /// case where there is nothing to inspect.
    static func items(configuration: MockConfiguration) -> [QuarantineItem] {
        configuration.quarantineSurfaces.enumerated().map { ordinal, surface in
            QuarantineItem(
                id: MockIdentifiers.quarantineID(seed: configuration.seed, ordinal: ordinal),
                surface: surface,
                reason: reason(for: surface),
                assetID: assetIdentifier(configuration: configuration, surface: surface, ordinal: ordinal),
                detectedAt: configuration.clock.offset(days: -ordinal - 1),
                preservedBytes: surface.storage.preservesOriginalBytes
                    ? UInt64(1_800_000 + ordinal * 640_000)
                    : nil,
                resolutions: resolutions(for: surface)
            )
        }
    }

    private static func reason(for surface: QuarantineSurface) -> QuarantineReason {
        switch surface {
        case .verifyAssetReject: .verifyRejected(.badWriteSig)
        case .federationSoftFail: .serverRejected(.federationCapabilityInvalid)
        case .malformedSidecar: .malformedEncoding
        case .orphanedOriginal: .serverRejected(.uploadStorageInconsistent)
        case .staleRevival: .staleProvenanceChain
        case .albumUpgradeStrandedWrite: .awaitingAlbumUpgrade
        case .backupRestoreChainConflict:
            .schemaAhead(SchemaAhead(surface: .sidecarSchema, found: "2", maxKnown: "1"))
        case .pendingDropAwaitingAdoption, .unknown: .awaitingReview
        }
    }

    /// ``QuarantineResolution/inspect`` is always offered; repair only where the
    /// preserved state makes repair mean something. There is deliberately no
    /// "resolve automatically" — automatic resolution *is* silently applying or
    /// silently dropping, which is the behaviour the whole surface prevents.
    private static func resolutions(for surface: QuarantineSurface) -> [QuarantineResolution] {
        surface.storage.preservesOriginalBytes
            ? [.inspect, .repair, .discard]
            : [.inspect, .discard]
    }

    /// Absent for a federation soft-fail on an unrecognised hash, and for a
    /// stranded write with no local asset yet — both are real cases the UI has
    /// to render without an asset to show.
    private static func assetIdentifier(
        configuration: MockConfiguration,
        surface: QuarantineSurface,
        ordinal: Int
    ) -> String? {
        switch surface {
        case .federationSoftFail, .albumUpgradeStrandedWrite: nil
        default: MockAssetRef(kind: .live, index: ordinal * 13).uuidString(seed: configuration.seed)
        }
    }
}

// MARK: - MockMaintenanceSeed

enum MockMaintenanceSeed {
    /// Every job, in the order a diagnostics screen lists them, with a spread of
    /// states so idle, completed-with-findings, failed, and
    /// waiting-for-conditions are all on screen at once.
    static func tasks(configuration: MockConfiguration) -> [MaintenanceTask] {
        let clock = configuration.clock
        return [
            MaintenanceTask(kind: .indexReconciliation, state: .idle, lastRunAt: clock.offset(days: -1)),
            MaintenanceTask(
                kind: .structuralValidation,
                state: .completed(occurredAt: clock.offset(days: -3), findingCount: 0),
                lastRunAt: clock.offset(days: -3)
            ),
            MaintenanceTask(
                kind: .deepContentValidation,
                state: .waitingForConditions,
                lastRunAt: clock.offset(days: -21)
            ),
            MaintenanceTask(
                kind: .intraLibraryDeduplication,
                state: .completed(occurredAt: clock.offset(days: -9), findingCount: 4),
                lastRunAt: clock.offset(days: -9)
            ),
            MaintenanceTask(kind: .cacheEviction, state: .idle, lastRunAt: clock.offset(days: -2)),
            MaintenanceTask(
                kind: .trashPurge,
                state: .failed(occurredAt: clock.offset(days: -5), code: .uploadStorageInconsistent),
                lastRunAt: clock.offset(days: -5)
            ),
        ]
    }
}
