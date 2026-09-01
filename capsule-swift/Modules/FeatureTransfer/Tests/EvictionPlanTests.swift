import Foundation
import Testing

import CapsuleDomain
import CapsuleMock
import FeatureTransfer

/// The documented sweep (*Filesystem — Client: Automatic cache management*).
@Suite("Eviction plan preview")
struct EvictionPlanTests {
    private func breakdown(
        original: UInt64 = 1000,
        preview: UInt64 = 200,
        thumbnail: UInt64 = 50,
        lqip: UInt64 = 5,
        unreleased: UInt64 = 0
    ) -> LocalStorageBreakdown {
        LocalStorageBreakdown(
            bytesByTier: [
                .original: original,
                .preview: preview,
                .thumbnail: thumbnail,
                .lqip: lqip,
                .dominantColour: 0,
            ],
            trashBytes: 0,
            unreleasedOriginalBytes: unreleased
        )
    }

    @Test("evicts in original → preview → thumbnail order")
    func tierOrder() {
        let plan = EvictionPlan.preview(targetBytes: 1250, breakdown: breakdown())

        #expect(plan.steps.map(\.tier) == [.original, .preview, .thumbnail])
        #expect(plan.steps.map(\.bytes) == [1000, 200, 50])
        #expect(plan.reclaimedBytes == 1250)
    }

    @Test("stops as soon as the target is met, without descending further")
    func stopsAtTarget() {
        let plan = EvictionPlan.preview(targetBytes: 600, breakdown: breakdown())

        #expect(plan.steps.map(\.tier) == [.original])
        #expect(plan.steps.first?.bytes == 600)
        #expect(plan.shortfallBytes == 0)
    }

    @Test("never reclaims the metadata tier, however large the target")
    func neverTouchesMetadata() {
        let plan = EvictionPlan.preview(targetBytes: .max, breakdown: breakdown())

        #expect(!plan.steps.contains { $0.tier == .lqip })
        #expect(!plan.steps.contains { $0.tier == .dominantColour })
        // An asset stays listable and previewable at LQIP fidelity after every
        // heavier representation is gone.
        #expect(plan.reclaimedBytes == 1250)
    }

    @Test("pinned bytes are held back from the original tier")
    func pinnedIsExempt() {
        let plan = EvictionPlan.preview(targetBytes: 1000, breakdown: breakdown(), pinnedBytes: 800)

        #expect(plan.steps.first { $0.tier == .original }?.bytes == 200)
        #expect(plan.exemptBytes == 800)
        #expect(plan.reclaimedBytes == 450)
        #expect(plan.shortfallBytes == 550)
    }

    @Test("device-owned originals not yet durable are exempt from the sweep")
    func unreleasedOriginalsAreExempt() {
        let plan = EvictionPlan.preview(targetBytes: 1000, breakdown: breakdown(unreleased: 1000))

        #expect(!plan.steps.contains { $0.tier == .original })
        #expect(plan.exemptBytes == 1000)
        #expect(plan.reclaimedBytes == 250)
    }

    @Test("an exempt total larger than the tier floors at zero rather than trapping")
    func exemptOverflowIsSafe() {
        let plan = EvictionPlan.preview(
            targetBytes: 100,
            breakdown: breakdown(original: 10, unreleased: 5000)
        )

        #expect(!plan.steps.contains { $0.tier == .original })
    }

    @Test("a target of zero plans nothing")
    func zeroTargetPlansNothing() {
        let plan = EvictionPlan.preview(targetBytes: 0, breakdown: breakdown())

        #expect(plan.isEmpty)
        #expect(plan.reclaimedBytes == 0)
    }

    @Test("the documented tier order is the one the plan uses")
    func documentedOrder() {
        #expect(EvictionPlan.tierOrder == [.original, .preview, .thumbnail])
    }
}

// MARK: - Model

@Suite("StorageReclamationModel against the mock")
@MainActor
struct StorageReclamationModelTests {
    @Test("previewing does not evict")
    func previewIsNonDestructive() async {
        let environment = MockEnvironment(scenario: .healthy)
        let model = StorageReclamationModel(
            storage: environment.storage,
            settings: environment.settings,
            sync: environment.sync
        )
        await model.reload()
        let before = model.breakdown.totalBytes

        model.previewEviction(targetBytes: 1000000)

        #expect(model.pendingPlan != nil)
        #expect(model.breakdown.totalBytes == before)
        #expect(model.lastReclaimedBytes == nil)
    }

    @Test("discarding the plan leaves nothing pending")
    func discardsPlan() async {
        let environment = MockEnvironment(scenario: .healthy)
        let model = StorageReclamationModel(
            storage: environment.storage,
            settings: environment.settings,
            sync: environment.sync
        )
        await model.reload()
        model.previewEviction(targetBytes: 1000000)

        model.discardPlan()

        #expect(model.pendingPlan == nil)
    }

    @Test("device-owned originals appear as an exempt consumer, never as a target")
    func exemptConsumerIsMarked() async {
        let environment = MockEnvironment(scenario: .awaitingOriginals)
        let model = StorageReclamationModel(
            storage: environment.storage,
            settings: environment.settings,
            sync: environment.sync
        )

        await model.reload()

        let exempt = model.consumers.filter(\.isExempt)
        #expect(!exempt.isEmpty)
        #expect(model.consumers.contains { $0.kind == .unreleasedOriginals })
    }
}
