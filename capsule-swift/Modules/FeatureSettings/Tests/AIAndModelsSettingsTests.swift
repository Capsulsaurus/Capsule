import CapsuleDomain
import CapsuleFoundation
import FeatureSettings
import Foundation
import Testing

// MARK: - AISlotReportTests

/// A search over a stale slot returns **fewer** results, and a UI that showed a
/// shrunken result set without saying why would read as data loss. So exclusion
/// is a reportable state, never a silent filter.
@Suite("A model slot's state is reported, exclusion included")
struct AISlotReportTests {
    private static let slot = ModelSlot(modelID: "clip-vit-b32", modelVersion: "1")
    private static let replacement = ModelSlot(modelID: "clip-vit-b32", modelVersion: "2")

    private static func report(_ availability: AIModelStatus.Availability, pending: Int = 0) -> AISlotReport {
        AISlotReport(
            AIModelStatus(slot: slot, purpose: .imageEmbedding, availability: availability, pendingAssetCount: pending)
        )
    }

    @Test("every port availability maps to a report, and each has its own status key")
    func everyAvailabilityIsReported() {
        let reports: [AISlotReport] = [
            Self.report(.ready, pending: 12),
            Self.report(.notDownloaded),
            Self.report(.downloading(fractionComplete: 0.25)),
            Self.report(.supersededBy(Self.replacement)),
            Self.report(.unsupportedOnThisDevice),
        ]

        #expect(reports[0] == .ready(pendingAssetCount: 12))
        #expect(reports[1] == .notDownloaded)
        #expect(reports[2] == .downloading(fractionComplete: 0.25))
        #expect(reports[3] == .staleExcluded(supersededBy: Self.replacement))
        #expect(reports[4] == .unsupportedOnThisDevice)
        #expect(Set(reports.map(\.statusKey)).count == 5)
        for report in reports {
            #expect(report.statusKey.hasPrefix("app.settings.ai.state."))
            #expect(!report.statusKey.contains(" "))
        }
    }

    @Test("only the superseded slot is excluded from queries")
    func onlyStaleSlotsAreExcluded() {
        #expect(Self.report(.supersededBy(Self.replacement)).isExcludedFromQueries)
        #expect(!Self.report(.ready).isExcludedFromQueries)
        #expect(!Self.report(.notDownloaded).isExcludedFromQueries)
        #expect(!Self.report(.unsupportedOnThisDevice).isExcludedFromQueries)
    }

    /// Weights are never in the repository, so "not downloaded" is a normal
    /// steady state and must not be drawn as a problem.
    @Test("a slot with no weights is neutral, not an error")
    func missingWeightsAreNotAnError() {
        #expect(Self.report(.notDownloaded).tone == .neutral)
        #expect(Self.report(.downloading(fractionComplete: 0.1)).tone == .neutral)
        #expect(Self.report(.ready).tone == .positive)
        #expect(Self.report(.supersededBy(Self.replacement)).tone == .caution)
        #expect(Self.report(.unsupportedOnThisDevice).tone == .caution)
    }
}

// MARK: - AIAndModelsSettingsTests

@Suite("AI & Models reports its slots, and clears a staleness only on request")
@MainActor
struct AIAndModelsSettingsTests {
    private static func model(
        intelligence: StubAIPort = StubAIPort(),
        settings: StubSettingsPort = StubSettingsPort(),
        connection: ConnectionClass? = .unmetered
    ) -> AIAndModelsSettingsModel {
        AIAndModelsSettingsModel(
            intelligence: intelligence,
            settings: settings,
            connectivity: .stub(connection: connection)
        )
    }

    @Test("loading reports the slots, the switch, and the power condition")
    func loadReportsTheSlots() async {
        let model = Self.model()

        await model.load()

        #expect(model.phase == .ready)
        #expect(model.statuses.count == 2)
        #expect(model.isProcessingEnabled)
        #expect(model.requiresPower)
        #expect(model.pendingAssetCount == 128)
        #expect(model.busySlot == nil)
    }

    @Test("a device with no model slots is empty rather than ready")
    func noSlotsIsEmpty() async {
        let model = Self.model(intelligence: StubAIPort(statuses: []))

        await model.load()

        #expect(model.phase == .empty)
        #expect(!model.hasStaleExclusions)
        #expect(model.pendingAssetCount == 0)
    }

    @Test("a slot list that cannot be read is classified, and offline wins over the code")
    func failedReadIsClassified() async {
        let failing = StubAIPort(readFailure: StubError.failure(.syncCursorInvalid))
        let model = Self.model(intelligence: failing)
        let offlineModel = Self.model(intelligence: failing, connection: .offline)

        await model.load()
        await offlineModel.load()

        #expect(model.phase == .failed(.syncCursorInvalid))
        #expect(offlineModel.phase == .offline)
    }

    /// Non-empty is a normal, temporary state after a model upgrade — the screen
    /// says so rather than letting search quietly return less.
    @Test("a superseded slot is listed as excluded from queries")
    func staleSlotsAreListed() async {
        let model = Self.model()

        await model.load()

        #expect(model.hasStaleExclusions)
        #expect(model.excludedSlots == [StubAIPort.embeddingSlot])
    }

    /// Nothing regenerates on its own, because regeneration walks the whole
    /// library re-running inference — a cost the user gets to choose.
    @Test("only an explicit regeneration clears a staleness")
    func regenerationIsTheOnlyWayOutOfStaleness() async {
        let port = StubAIPort()
        let model = Self.model(intelligence: port)
        await model.load()
        #expect(model.hasStaleExclusions)

        await model.regenerate(StubAIPort.embeddingSlot)

        #expect(!model.hasStaleExclusions)
        #expect(model.excludedSlots.isEmpty)
        #expect(model.busySlot == nil, "the busy marker is cleared when the run ends")
        let regenerated = await port.regeneratedSlots
        #expect(regenerated == [StubAIPort.embeddingSlot])
    }

    @Test("downloading weights moves the slot to ready and touches nothing else")
    func downloadingMovesOneSlot() async {
        let port = StubAIPort()
        let model = Self.model(intelligence: port)
        await model.load()

        await model.download(StubAIPort.embeddingSlot)

        let downloaded = await port.downloadedSlots
        #expect(downloaded == [StubAIPort.embeddingSlot])
        #expect(model.statuses.count == 2)
        let face = model.statuses.first { $0.slot == StubAIPort.faceSlot }
        #expect(face?.availability == .ready)
    }

    /// Output from a slot with no model is unverifiable, so removing the model
    /// removes the output — which is honest, and is why it is confirmed at the
    /// call site rather than being a casual tap.
    @Test("removing a model removes its row, and the removal is a deliberate call")
    func removingAModelDropsItsSlot() async {
        let port = StubAIPort()
        let model = Self.model(intelligence: port)
        await model.load()

        await model.remove(StubAIPort.faceSlot)

        let removed = await port.removedSlots
        #expect(removed == [StubAIPort.faceSlot])
        #expect(model.statuses.map(\.slot) == [StubAIPort.embeddingSlot])
    }

    @Test("a removal that fails leaves the slot listed and says why")
    func failedRemovalIsSurfaced() async {
        let port = StubAIPort(writeFailure: StubError.failure(.storageInvalidRequest))
        let model = Self.model(intelligence: port)
        await model.load()

        await model.remove(StubAIPort.faceSlot)

        #expect(model.phase == .failed(.storageInvalidRequest))
        #expect(model.statuses.count == 2, "a failed removal must not look like a successful one")
    }

    @Test("the processing switch is written and read back rather than assumed")
    func processingSwitchRoundTrips() async {
        let port = StubAIPort()
        let model = Self.model(intelligence: port)
        await model.load()
        #expect(model.isProcessingEnabled)

        await model.setProcessingEnabled(false)

        #expect(!model.isProcessingEnabled)
    }

    @Test("a processing write that fails does not flip the switch on screen")
    func failedProcessingWriteDoesNotFlipTheSwitch() async {
        let port = StubAIPort(writeFailure: StubError.failure(.storageInvalidRequest))
        let model = Self.model(intelligence: port)
        await model.load()

        await model.setProcessingEnabled(false)

        #expect(model.phase == .failed(.storageInvalidRequest))
        #expect(model.isProcessingEnabled, "the screen must not claim a setting that was refused")
    }

    @Test("the power condition is stored in the settings document")
    func powerConditionIsPersisted() async {
        let settings = StubSettingsPort(document: LibrarySettings(aiRequiresPower: true))
        let model = Self.model(settings: settings)
        await model.load()

        await model.setRequiresPower(false)

        #expect(!model.requiresPower)
        let stored = await settings.storedDocument
        #expect(!stored.aiRequiresPower)
    }

    @Test("a power-condition write that fails leaves the screen honest")
    func failedPowerWriteIsSurfaced() async {
        let settings = StubSettingsPort(
            document: LibrarySettings(aiRequiresPower: true),
            writeFailure: StubError.failure(.storageInvalidRequest)
        )
        let model = Self.model(settings: settings)
        await model.load()

        await model.setRequiresPower(false)

        #expect(model.phase == .failed(.storageInvalidRequest))
        #expect(model.requiresPower)
    }
}
