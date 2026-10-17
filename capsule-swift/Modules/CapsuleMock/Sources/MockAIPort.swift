import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation

// MARK: - AIPort

extension MockIntelligenceStore: AIPort {
    public func modelStatuses() async throws -> [AIModelStatus] {
        statusList
    }

    /// Fetch a model's weights, streaming progress.
    ///
    /// Progress is emitted in fixed steps rather than on a timer, so a test can
    /// consume the whole stream deterministically and a demo shows the same
    /// sequence every time.
    public nonisolated func downloadModel(slot: ModelSlot) -> AsyncStream<AIModelStatus> {
        AsyncStream { continuation in
            Task {
                for step in 1 ... 8 {
                    let fraction = Double(step) / 8
                    let status = await self.advanceDownload(slot: slot, fraction: fraction)
                    continuation.yield(status)
                }
                continuation.finish()
            }
        }
    }

    /// Delete a model's weights **and everything derived from that slot**.
    ///
    /// The only honest way to undo it: output from a slot with no model is
    /// unverifiable, so keeping the tags while dropping the weights would leave
    /// the library asserting things nothing can check.
    public func removeModel(slot: ModelSlot) async throws {
        removeSlot(slot)
        await modelChanges.send(statusList)
        await peopleChanges.send(())
    }

    public func isProcessingEnabled() async -> Bool {
        processingEnabled
    }

    public func setProcessingEnabled(_ enabled: Bool) async throws {
        updateProcessingEnabled(enabled)
        await modelChanges.send(statusList)
    }

    /// Re-run a slot over the assets whose output went stale after a model
    /// change.
    public nonisolated func regenerate(slot: ModelSlot) -> AsyncStream<AIModelStatus> {
        AsyncStream { continuation in
            Task {
                let total = await self.pendingCount(slot: slot)
                for step in stride(from: total, through: 0, by: -max(1, total / 6)) {
                    let status = await self.setPending(slot: slot, count: step)
                    continuation.yield(status)
                }
                let finished = await self.setPending(slot: slot, count: 0)
                continuation.yield(finished)
                continuation.finish()
            }
        }
    }

    public nonisolated func changes() -> AsyncStream<[AIModelStatus]> {
        modelChanges.subscribe()
    }

    // MARK: Progress

    private func advanceDownload(slot: ModelSlot, fraction: Double) async -> AIModelStatus {
        let existing = statusList.first { $0.slot == slot }
        let status = AIModelStatus(
            slot: slot,
            purpose: existing?.purpose ?? .sceneTagging,
            availability: fraction >= 1 ? .ready : .downloading(fractionComplete: fraction),
            pendingAssetCount: existing?.pendingAssetCount ?? 0
        )
        setStatus(status)
        await modelChanges.send(statusList)
        return status
    }

    private func pendingCount(slot: ModelSlot) -> Int {
        statusList.first { $0.slot == slot }?.pendingAssetCount ?? 0
    }

    private func setPending(slot: ModelSlot, count: Int) async -> AIModelStatus {
        let existing = statusList.first { $0.slot == slot }
        let status = AIModelStatus(
            slot: slot,
            purpose: existing?.purpose ?? .sceneTagging,
            availability: existing?.availability ?? .ready,
            pendingAssetCount: max(0, count)
        )
        setStatus(status)
        await modelChanges.send(statusList)
        return status
    }
}
