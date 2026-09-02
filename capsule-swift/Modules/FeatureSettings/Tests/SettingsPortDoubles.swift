import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

// MARK: - StubAIPort

/// An ``AIPort`` over a fixed slot table.
///
/// The stale-exclusion state is the one this double exists for: a model swap
/// leaves old embeddings excluded from queries until regenerated, and a screen
/// that could not reach that state would leave the "search is returning less,
/// and here is why" copy untested.
actor StubAIPort: AIPort {
    private var slotStatuses: [AIModelStatus]
    private var processingEnabled: Bool
    private let readFailure: CapsuleError?
    private let writeFailure: CapsuleError?
    private(set) var regeneratedSlots: [ModelSlot] = []
    private(set) var downloadedSlots: [ModelSlot] = []
    private(set) var removedSlots: [ModelSlot] = []

    init(
        statuses: [AIModelStatus] = StubAIPort.defaultStatuses,
        processingEnabled: Bool = true,
        readFailure: CapsuleError? = nil,
        writeFailure: CapsuleError? = nil
    ) {
        slotStatuses = statuses
        self.processingEnabled = processingEnabled
        self.readFailure = readFailure
        self.writeFailure = writeFailure
    }

    static let embeddingSlot = ModelSlot(modelID: "clip-vit-b32", modelVersion: "1")
    static let supersedingSlot = ModelSlot(modelID: "clip-vit-b32", modelVersion: "2")
    static let faceSlot = ModelSlot(modelID: "face-detect", modelVersion: "3")

    /// One ready slot and one superseded slot, so both the ordinary and the
    /// excluded rows are drawn.
    static let defaultStatuses: [AIModelStatus] = [
        AIModelStatus(
            slot: embeddingSlot,
            purpose: .imageEmbedding,
            availability: .supersededBy(supersedingSlot),
            pendingAssetCount: 120
        ),
        AIModelStatus(
            slot: faceSlot,
            purpose: .faceDetection,
            availability: .ready,
            pendingAssetCount: 8
        ),
    ]

    func modelStatuses() async throws -> [AIModelStatus] {
        if let readFailure { throw readFailure }
        return slotStatuses
    }

    nonisolated func downloadModel(slot: ModelSlot) -> AsyncStream<AIModelStatus> {
        AsyncStream { continuation in
            Task {
                await self.record(download: slot)
                continuation.yield(
                    AIModelStatus(slot: slot, purpose: .imageEmbedding, availability: .downloading(fractionComplete: 0.5))
                )
                await self.settle(slot: slot, to: .ready)
                continuation.yield(AIModelStatus(slot: slot, purpose: .imageEmbedding, availability: .ready))
                continuation.finish()
            }
        }
    }

    func removeModel(slot: ModelSlot) async throws {
        if let writeFailure { throw writeFailure }
        removedSlots.append(slot)
        slotStatuses.removeAll { $0.slot == slot }
    }

    func isProcessingEnabled() async -> Bool { processingEnabled }

    func setProcessingEnabled(_ enabled: Bool) async throws {
        if let writeFailure { throw writeFailure }
        processingEnabled = enabled
    }

    nonisolated func regenerate(slot: ModelSlot) -> AsyncStream<AIModelStatus> {
        AsyncStream { continuation in
            Task {
                await self.record(regenerate: slot)
                await self.settle(slot: slot, to: .ready)
                continuation.yield(
                    AIModelStatus(slot: slot, purpose: .imageEmbedding, availability: .ready)
                )
                continuation.finish()
            }
        }
    }

    nonisolated func changes() -> AsyncStream<[AIModelStatus]> {
        AsyncStream { $0.finish() }
    }

    private func record(download slot: ModelSlot) {
        downloadedSlots.append(slot)
    }

    private func record(regenerate slot: ModelSlot) {
        regeneratedSlots.append(slot)
    }

    /// Only regeneration clears a staleness — nothing here does it on its own.
    private func settle(slot: ModelSlot, to availability: AIModelStatus.Availability) {
        guard let index = slotStatuses.firstIndex(where: { $0.slot == slot }) else { return }
        slotStatuses[index].availability = availability
        slotStatuses[index].pendingAssetCount = 0
    }
}

// MARK: - StubMaintenancePort

/// A ``MaintenancePort`` that records what was asked to run.
///
/// The refusal being testable is the point: whole-library deduplication is a
/// user-initiated action, never an automatic background deletion, so the double
/// has to be able to say whether it was started at all.
actor StubMaintenancePort: MaintenancePort {
    private var taskList: [MaintenanceTask]
    private let readFailure: CapsuleError?
    private(set) var startedKinds: [MaintenanceTaskKind] = []
    private(set) var cancelledKinds: [MaintenanceTaskKind] = []

    init(tasks: [MaintenanceTask] = StubMaintenancePort.defaultTasks, readFailure: CapsuleError? = nil) {
        taskList = tasks
        self.readFailure = readFailure
    }

    static let defaultTasks: [MaintenanceTask] = [
        MaintenanceTask(
            kind: .indexReconciliation,
            state: .completed(occurredAt: SettingsInstant.days(-1), findingCount: 0)
        ),
        MaintenanceTask(kind: .cacheEviction, state: .idle),
        MaintenanceTask(
            kind: .intraLibraryDeduplication,
            state: .completed(occurredAt: SettingsInstant.days(-3), findingCount: 4)
        ),
    ]

    func tasks() async throws -> [MaintenanceTask] {
        if let readFailure { throw readFailure }
        return taskList
    }

    nonisolated func run(_ kind: MaintenanceTaskKind) -> AsyncStream<MaintenanceTask> {
        AsyncStream { continuation in
            Task {
                await self.record(started: kind)
                continuation.yield(MaintenanceTask(kind: kind, state: .running(fractionComplete: 0.5)))
                let finished = MaintenanceTask(
                    kind: kind,
                    state: .completed(occurredAt: SettingsInstant.reference, findingCount: 0),
                    lastRunAt: SettingsInstant.reference
                )
                await self.apply(finished)
                continuation.yield(finished)
                continuation.finish()
            }
        }
    }

    func cancel(_ kind: MaintenanceTaskKind) async throws {
        cancelledKinds.append(kind)
        apply(MaintenanceTask(kind: kind, state: .idle))
    }

    nonisolated func changes() -> AsyncStream<[MaintenanceTask]> {
        AsyncStream { $0.finish() }
    }

    private func record(started kind: MaintenanceTaskKind) {
        startedKinds.append(kind)
    }

    private func apply(_ task: MaintenanceTask) {
        guard let index = taskList.firstIndex(where: { $0.kind == task.kind }) else {
            taskList.append(task)
            return
        }
        taskList[index] = task
    }
}

// MARK: - StubStoragePort

/// A ``StoragePort`` over a fixed local breakdown.
actor StubStoragePort: StoragePort {
    private var breakdown: LocalStorageBreakdown
    private let readFailure: CapsuleError?
    private let evictFailure: CapsuleError?
    private(set) var evictionTargets: [UInt64] = []

    init(
        breakdown: LocalStorageBreakdown = StubStoragePort.defaultBreakdown,
        readFailure: CapsuleError? = nil,
        evictFailure: CapsuleError? = nil
    ) {
        self.breakdown = breakdown
        self.readFailure = readFailure
        self.evictFailure = evictFailure
    }

    static let defaultBreakdown = LocalStorageBreakdown(
        bytesByTier: [
            .dominantColour: 1024,
            .lqip: 2048,
            .thumbnail: 4000000,
            .preview: 12000000,
            .original: 40000000,
        ],
        trashBytes: 1000000,
        unreleasedOriginalBytes: 30000000,
        availableDiskBytes: 90000000000
    )

    func localBreakdown() async throws -> LocalStorageBreakdown {
        if let readFailure { throw readFailure }
        return breakdown
    }

    func verify(assetIDs _: [AssetID], deep _: Bool) async throws -> [StorageVerification] { [] }

    func releaseLocalCopies(for _: [AssetID]) async throws {}

    func evictCache(targetBytes: UInt64) async throws -> UInt64 {
        if let evictFailure { throw evictFailure }
        evictionTargets.append(targetBytes)
        // Only the re-fetchable tiers move; the unreleased originals do not.
        let released = min(targetBytes, breakdown.reclaimableBytes)
        breakdown.bytesByTier[.preview] = 0
        return released
    }

    func setPinned(_: Bool, for _: [AssetID]) async throws {}
}
