import Foundation
import Testing

import CapsuleDomain
import CapsuleMock
import FeatureTransfer

/// The five quota states and what each one permits
/// (*Quota — Thresholds and States*).
@Suite("Quota states and permitted actions")
struct QuotaPermissionTests {
    @Test("within quota: everything works and nothing is offered")
    func withinQuota() {
        let permissions = QuotaPermissions.forState(.withinQuota)

        #expect(permissions.newUploads)
        #expect(permissions.metadataGrowth)
        #expect(permissions.reclaimingWrites)
        #expect(QuotaPermissions.remediations(for: .withinQuota).isEmpty)
    }

    @Test("soft warning: uploads still succeed, the UI warns")
    func softWarning() {
        let permissions = QuotaPermissions.forState(.softWarning)

        #expect(permissions.newUploads)
        #expect(permissions.metadataGrowth)
        #expect(QuotaPermissions.remediations(for: .softWarning) == [.emptyTrash, .reviewLargest])
    }

    @Test("hard exceeded: new uploads are refused, every other write still works")
    func hardExceeded() {
        let permissions = QuotaPermissions.forState(.hardExceeded)

        #expect(!permissions.newUploads)
        #expect(permissions.metadataGrowth)
        #expect(permissions.reclaimingWrites)
    }

    @Test("grace expired: metadata growth stops but deleting never does")
    func graceExpired() {
        let permissions = QuotaPermissions.forState(.graceExpired)

        #expect(!permissions.newUploads)
        #expect(!permissions.metadataGrowth)
        // The product promise: a user can always delete their way back under
        // quota. Losing this would turn a full account into a permanent one.
        #expect(permissions.reclaimingWrites)
        #expect(QuotaPermissions.remediations(for: .graceExpired) == [.emptyTrash, .reviewLargest])
    }

    @Test("suspended: no reclaiming writes, so only the administrator is offered")
    func suspended() {
        let permissions = QuotaPermissions.forState(.suspended)

        #expect(!permissions.reclaimingWrites)
        #expect(QuotaPermissions.remediations(for: .suspended) == [.contactAdministrator])
    }

    @Test("every warned state offers at least one way out")
    func everyWarnedStateIsRemediable() {
        let warned = QuotaState.knownCases.filter(\.warrantsWarning)

        #expect(!warned.isEmpty)
        #expect(warned.allSatisfy { !QuotaPermissions.remediations(for: $0).isEmpty })
    }
}

// MARK: - Category breakdown

/// The stacked bar, with the trash segment broken out.
@Suite("Quota category breakdown")
struct QuotaCategoryBreakdownTests {
    private func quota(used: UInt64) -> QuotaStatus {
        QuotaStatus(used: used, softLimit: 800, hardLimit: 1000, state: .softWarning)
    }

    @Test("trash is taken verbatim — it is the one exact number available")
    func trashIsExact() {
        let local = LocalStorageBreakdown(
            bytesByTier: [.original: 400, .preview: 100, .thumbnail: 20, .lqip: 5],
            trashBytes: 120
        )

        let breakdown = QuotaCategoryBreakdown.make(quota: quota(used: 600), local: local)

        #expect(breakdown.segments.first { $0.category == .trash }?.bytes == 120)
    }

    @Test("the remainder is split across the categories the device can see")
    func splitsRemainder() {
        let local = LocalStorageBreakdown(
            bytesByTier: [.original: 300, .preview: 100, .thumbnail: 0, .lqip: 0],
            trashBytes: 100
        )

        let breakdown = QuotaCategoryBreakdown.make(quota: quota(used: 500), local: local)
        let total = breakdown.segments.reduce(UInt64.zero) { $0 + $1.bytes }

        #expect(total == 500)
        #expect(breakdown.segments.first { $0.category == .originals }?.bytes == 300)
        #expect(breakdown.segments.first { $0.category == .derivatives }?.bytes == 100)
        #expect(breakdown.isEstimated)
    }

    @Test("charged bytes this device cannot attribute are shown, not folded away")
    func unattributedBytes() {
        let breakdown = QuotaCategoryBreakdown.make(
            quota: quota(used: 500),
            local: LocalStorageBreakdown()
        )

        #expect(breakdown.segments.map(\.category) == [.other])
        #expect(breakdown.segments.first?.bytes == 500)
        #expect(!breakdown.isEstimated)
    }

    @Test("an unlimited deployment reports no free-space figure to scale against")
    func unlimited() {
        let unlimited = QuotaStatus(used: 500, softLimit: 0, hardLimit: .max, state: .withinQuota)

        let breakdown = QuotaCategoryBreakdown.make(quota: unlimited, local: LocalStorageBreakdown())

        #expect(breakdown.freeBytes == 0)
    }
}

// MARK: - Model

@Suite("QuotaStatusModel against the mock")
@MainActor
struct QuotaStatusModelTests {
    @Test("the grace-expired scenario is remediable, not opaque")
    func graceExpiredIsRemediable() async {
        let environment = MockEnvironment(scenario: .quotaGraceExpired)
        let model = QuotaStatusModel(
            quota: environment.quota,
            storage: environment.storage,
            sync: environment.sync,
            clock: .fixed(environment.configuration.clock.now)
        )

        await model.reload()

        #expect(model.quota.state == .graceExpired)
        #expect(model.warrants)
        #expect(model.remediations.contains(.emptyTrash))
        #expect(model.permissions.reclaimingWrites)
        #expect(model.phase == .ready)
    }

    @Test("a healthy account warns about nothing")
    func healthyDoesNotWarn() async {
        let environment = MockEnvironment(scenario: .healthy)
        let model = QuotaStatusModel(
            quota: environment.quota,
            storage: environment.storage,
            sync: environment.sync,
            clock: .fixed(environment.configuration.clock.now)
        )

        await model.reload()

        #expect(model.quota.state == .withinQuota)
        #expect(!model.warrants)
        #expect(model.remediations.isEmpty)
    }

    @Test("offline is a phase of its own, not a failure")
    func offlineIsAPhase() async {
        let environment = MockEnvironment(scenario: .offline)
        let model = QuotaStatusModel(
            quota: environment.quota,
            storage: environment.storage,
            sync: environment.sync,
            clock: .fixed(environment.configuration.clock.now)
        )

        await model.reload()

        #expect(model.phase == .offline)
        #expect(model.phase.hasContent)
        #expect(!model.phase.permitsNetworkActions)
    }
}
