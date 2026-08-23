import CapsuleDomain
import CapsuleFoundation
import FeatureSettings
import Foundation
import Testing

/// One gibibyte, typed. A bare `24 * 1_073_741_824` beside a `UInt64?` is
/// inferred as `Int` and silently compared through `AnyHashable`, which is
/// never equal — so the unit is a constant rather than an expression.
private let gibibyte: UInt64 = 1073741824

// MARK: - StorageSettingsTests

/// Quota is what the *server* is charging; local storage is what this *device*
/// holds and whether it is safe to stop holding it. Conflating them is how a
/// cache-clearing feature ends up deleting an only copy.
@Suite("Storage separates what the server charges from what this device holds")
@MainActor
struct StorageSettingsTests {
    private static func model(
        storage: StubStoragePort = StubStoragePort(),
        quota: StubQuotaPort = StubQuotaPort(),
        settings: StubSettingsPort = StubSettingsPort(
            document: LibrarySettings(cacheBudgetBytes: 24 * gibibyte)
        ),
        connection: ConnectionClass? = .unmetered
    ) -> StorageSettingsModel {
        StorageSettingsModel(
            storage: storage,
            quota: quota,
            settings: settings,
            connectivity: .stub(connection: connection)
        )
    }

    @Test("loading reports the local breakdown, the quota, and the budget")
    func loadReportsAllThree() async {
        let model = Self.model()

        await model.load()

        #expect(model.phase == .ready)
        #expect(model.quota?.state == .withinQuota)
        #expect(model.cacheBudgetBytes == 24 * gibibyte)
        #expect(model.bytes(for: .original) == 40000000)
        #expect(model.bytes(for: .thumbnail) == 4000000)
    }

    @Test("the tiers are listed cheapest rung first")
    func tiersAreOrdered() {
        let model = Self.model()

        #expect(model.tiers == [.dominantColour, .lqip, .thumbnail, .preview, .original])
        #expect(RepresentationTier.allCases.count == 5)
    }

    @Test("a tier this device holds nothing for reads as zero rather than as missing")
    func absentTierReadsAsZero() async {
        let empty = StubStoragePort(breakdown: LocalStorageBreakdown())
        let model = Self.model(storage: empty)

        await model.load()

        #expect(model.bytes(for: .original) == 0)
        #expect(model.reclaimableBytes == 0)
        #expect(model.unreleasedOriginalBytes == 0)
    }

    /// The answer to "why did clearing the cache not free much": the device is
    /// the sole durable copy of those bytes.
    @Test("bytes the server has not confirmed durable are never counted as reclaimable")
    func unreleasedOriginalsAreNotReclaimable() async {
        let model = Self.model()

        await model.load()

        let total: UInt64 = 1024 + 2048 + 4000000 + 12000000 + 40000000
        #expect(model.unreleasedOriginalBytes == 30000000)
        #expect(model.reclaimableBytes == total - 30000000)
    }

    @Test("a storage read that fails is classified, and offline wins over the code")
    func failedReadIsClassified() async {
        let failing = StubStoragePort(readFailure: StubError.failure(.storageInvalidRequest))
        let model = Self.model(storage: failing)
        let offlineModel = Self.model(storage: failing, connection: .offline)

        await model.load()
        await offlineModel.load()

        #expect(model.phase == .failed(.storageInvalidRequest))
        #expect(offlineModel.phase == .offline)
        #expect(model.breakdown == nil)
    }

    @Test("a quota that cannot be read fails the screen rather than showing a guess")
    func failedQuotaFailsTheScreen() async {
        let model = Self.model(quota: StubQuotaPort(quota: nil))

        await model.load()

        #expect(model.phase == .failed(.quotaExceeded))
        #expect(model.quota == nil)
    }

    @Test("the budget options are a short list of real disk figures")
    func budgetOptionsAreABoundedList() {
        let options = StorageSettingsModel.budgetOptions

        #expect(options.count == 6)
        #expect(options == options.sorted())
        #expect(options.first == 4 * gibibyte)
        #expect(Set(options).count == options.count)
    }

    @Test("setting a budget writes it and reads it back; clearing it is its own value")
    func budgetCanBeSetAndCleared() async {
        let port = StubSettingsPort(document: LibrarySettings(cacheBudgetBytes: 24 * gibibyte))
        let model = Self.model(settings: port)
        await model.load()

        await model.setCacheBudget(8 * gibibyte)
        #expect(model.cacheBudgetBytes == 8 * gibibyte)

        await model.setCacheBudget(nil)

        #expect(model.cacheBudgetBytes == nil, "an unset budget is not a zero budget")
        let stored = await port.storedDocument
        #expect(stored.cacheBudgetBytes == nil)
    }

    @Test("a budget write that fails leaves the screen honest about it")
    func failedBudgetWriteIsSurfaced() async {
        let port = StubSettingsPort(
            document: LibrarySettings(cacheBudgetBytes: 24 * gibibyte),
            writeFailure: StubError.failure(.storageInvalidRequest)
        )
        let model = Self.model(settings: port)
        await model.load()

        await model.setCacheBudget(8 * gibibyte)

        #expect(model.phase == .failed(.storageInvalidRequest))
        #expect(model.cacheBudgetBytes == 24 * gibibyte, "the screen must not claim a write that did not happen")
        #expect(!model.isWorking)
    }

    @Test("eviction reports what it actually released and re-reads the breakdown")
    func evictionReportsWhatItReleased() async {
        let port = StubStoragePort()
        let model = Self.model(storage: port)
        await model.load()
        let before = model.bytes(for: .preview)

        await model.evictCache(targetBytes: 5000000)

        #expect(model.lastReclaimedBytes == 5000000)
        #expect(before == 12000000)
        #expect(model.bytes(for: .preview) == 0, "the breakdown is re-read rather than assumed")
        let targets = await port.evictionTargets
        #expect(targets == [5000000])
    }

    @Test("eviction cannot release more than is reclaimable")
    func evictionIsBoundedByWhatIsReclaimable() async {
        let model = Self.model()
        await model.load()

        await model.evictCache(targetBytes: UInt64.max)

        let total: UInt64 = 1024 + 2048 + 4000000 + 12000000 + 40000000
        #expect(model.lastReclaimedBytes == total - 30000000)
    }

    @Test("an eviction that fails leaves the last figure untouched")
    func failedEvictionIsSurfaced() async {
        let failing = StubStoragePort(evictFailure: StubError.failure(.storageInvalidRequest))
        let model = Self.model(storage: failing)
        await model.load()

        await model.evictCache(targetBytes: 5000000)

        #expect(model.phase == .failed(.storageInvalidRequest))
        #expect(model.lastReclaimedBytes == nil, "nothing was released, so nothing is reported as released")
    }

    @Test("every quota state has its own name and a tone that does not soften it")
    func quotaStatesAreNamedAndToned() {
        let states: [QuotaState] = [.withinQuota, .softWarning, .hardExceeded, .graceExpired, .suspended]
        let keys = states.map(\.titleKey)

        #expect(Set(keys).count == states.count)
        #expect(QuotaState.withinQuota.tone == .positive)
        #expect(QuotaState.softWarning.tone == .caution)
        #expect(QuotaState.graceExpired.tone == .critical)
        #expect(QuotaState.suspended.tone == .critical)
        #expect(QuotaState(rawValue: "throttled").tone == .neutral)
    }

    @Test("every representation tier is named by its own catalog key", arguments: RepresentationTier.allCases)
    func everyTierIsNamed(tier: RepresentationTier) {
        #expect(tier.titleKey.hasPrefix("ios.settings.storage.tier."))
        #expect(!tier.titleKey.contains(" "))
    }
}
