import CapsuleDomain
import CapsuleFoundation
import FeatureSettings
import Foundation
import Testing

// MARK: - MaintenanceSettingsTests

/// Whole-library deduplication is a user-initiated action or a surfaced
/// suggestion — **never an automatic background deletion**. A comment saying
/// "don't call this automatically" would have been a comment; the model makes
/// it a refusal, and this suite is what holds it to that.
@Suite("Deduplication runs only when a human asks for it")
@MainActor
struct MaintenanceSettingsTests {
    private static func model(
        _ port: StubMaintenancePort = StubMaintenancePort(),
        connection: ConnectionClass? = .unmetered
    ) -> MaintenanceSettingsModel {
        MaintenanceSettingsModel(maintenance: port, connectivity: .stub(connection: connection))
    }

    @Test("loading lists the scheduled jobs and what each last found")
    func loadListsTheJobs() async {
        let model = Self.model()

        await model.load()

        #expect(model.phase == .ready)
        #expect(model.tasks.count == 3)
        #expect(model.task(.cacheEviction)?.state == .idle)
        #expect(model.pendingDuplicateSetCount == 4)
    }

    @Test("an account with no scheduled jobs is empty rather than ready")
    func noJobsIsEmpty() async {
        let model = Self.model(StubMaintenancePort(tasks: []))

        await model.load()

        #expect(model.phase == .empty)
        #expect(model.tasks.isEmpty)
        #expect(model.pendingDuplicateSetCount == nil)
    }

    @Test("a job list that cannot be read is classified, and offline wins over the code")
    func failedReadIsClassified() async {
        let failing = StubMaintenancePort(readFailure: StubError.failure(.storageInvalidRequest))
        let model = Self.model(failing)
        let offlineModel = Self.model(failing, connection: .offline)

        await model.load()
        await offlineModel.load()

        #expect(model.phase == .failed(.storageInvalidRequest))
        #expect(offlineModel.phase == .offline)
    }

    /// The refusal is visible rather than a silently dropped call, so it shows
    /// up in a diagnostics report as well as in a test.
    @Test("an automatic sweep is refused deduplication, and the refusal is recorded")
    func automaticSweepCannotDeduplicate() async {
        let port = StubMaintenancePort()
        let model = Self.model(port)
        await model.load()

        let started = await model.run(.intraLibraryDeduplication, userInitiated: false)

        #expect(!started)
        #expect(model.refusedAutomaticKinds.contains(.intraLibraryDeduplication))
        #expect(!model.didStartDeduplication)
        let attempted = await port.startedKinds
        #expect(attempted.isEmpty, "a refused job must not reach the port at all")
    }

    @Test("a human pressing the button is what starts deduplication")
    func userInitiatedDeduplicationRuns() async {
        let port = StubMaintenancePort()
        let model = Self.model(port)
        await model.load()

        let started = await model.run(.intraLibraryDeduplication, userInitiated: true)

        #expect(started)
        #expect(model.didStartDeduplication)
        #expect(model.refusedAutomaticKinds.isEmpty)
        let attempted = await port.startedKinds
        #expect(attempted == [.intraLibraryDeduplication])
    }

    @Test("the scheduled sweep runs the harmless jobs and skips the gated one")
    func scheduledSweepSkipsTheGatedJob() async {
        let port = StubMaintenancePort()
        let model = Self.model(port)
        await model.load()

        await model.runScheduledSweep()

        let attempted = await port.startedKinds
        #expect(attempted == [.indexReconciliation, .cacheEviction])
        #expect(!attempted.contains(.intraLibraryDeduplication))
        #expect(!model.didStartDeduplication)
        #expect(model.refusedAutomaticKinds.isEmpty, "a sweep that never asks is not a refusal")
    }

    @Test("only deduplication carries the user-initiated gate", arguments: MaintenanceTaskKind.knownCases)
    func onlyDeduplicationIsGated(kind: MaintenanceTaskKind) async {
        let gated = MaintenanceSettingsModel.userInitiatedOnlyKinds.contains(kind)
        let port = StubMaintenancePort(tasks: [MaintenanceTask(kind: kind, state: .idle)])
        let model = Self.model(port)
        await model.load()

        let started = await model.run(kind, userInitiated: false)

        #expect(started == !gated)
        #expect(gated == (kind == .intraLibraryDeduplication))
    }

    /// Findings are candidates, never actions: a non-zero count means there is a
    /// decision waiting, not that anything was merged.
    @Test("a duplicate count is a decision waiting, and a run that found none clears it")
    func duplicateFindingsAreCandidates() async {
        let model = Self.model()
        await model.load()
        #expect(model.pendingDuplicateSetCount == 4)

        await model.run(.intraLibraryDeduplication, userInitiated: true)

        #expect(model.pendingDuplicateSetCount == 0)
    }

    @Test("running a job walks it through its states and leaves it completed")
    func runningAJobUpdatesItsRow() async {
        let model = Self.model()
        await model.load()

        await model.run(.cacheEviction, userInitiated: true)

        #expect(model.task(.cacheEviction)?.state.isRunning == false)
        #expect(model.task(.cacheEviction)?.lastRunAt == SettingsInstant.reference)
        #expect(model.startedKinds == [.cacheEviction])
    }

    @Test("cancelling a job stops it without rolling back what it did")
    func cancellingStopsTheJob() async {
        let port = StubMaintenancePort()
        let model = Self.model(port)
        await model.load()

        await model.cancel(.indexReconciliation)

        let cancelled = await port.cancelledKinds
        #expect(cancelled == [.indexReconciliation])
        #expect(model.task(.indexReconciliation)?.state == .idle)
    }

    @Test("every job kind is named and explained by its own catalog keys", arguments: MaintenanceTaskKind.knownCases)
    func everyJobIsNamed(kind: MaintenanceTaskKind) {
        #expect(kind.titleKey.hasPrefix("ios.settings.maintenance.job."))
        #expect(kind.detailKey.hasPrefix("ios.settings.maintenance.detail."))
        #expect(!kind.titleKey.contains(" "))
        #expect(kind.titleKey != MaintenanceTaskKind.unknown("x").titleKey)
    }

    @Test("a completed run with findings is drawn as a caution, one with none as reassurance")
    func completionToneFollowsTheFindings() {
        let clean = MaintenanceTask.State.completed(occurredAt: SettingsInstant.reference, findingCount: 0)
        let dirty = MaintenanceTask.State.completed(occurredAt: SettingsInstant.reference, findingCount: 4)

        #expect(clean.tone == .positive)
        #expect(dirty.tone == .caution)
        #expect(MaintenanceTask.State.failed(occurredAt: SettingsInstant.reference, code: .storageInvalidRequest).tone == .critical)
        #expect(MaintenanceTask.State.running(fractionComplete: 0.5).isRunning)
        #expect(!MaintenanceTask.State.idle.isRunning)
        let keys = [
            MaintenanceTask.State.idle,
            .running(fractionComplete: 0),
            clean,
            .failed(occurredAt: SettingsInstant.reference, code: .storageInvalidRequest),
            .waitingForConditions,
        ].map(\.statusKey)
        #expect(Set(keys).count == 5)
    }
}
