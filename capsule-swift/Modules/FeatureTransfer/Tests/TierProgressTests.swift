import Foundation
import Testing

import CapsuleDomain
import CapsuleFoundation
import FeatureTransfer

/// The staged-upload ladder as the header ring renders it
/// (*Download and Synchronization — Upload Tiering*).
@Suite("Tier progress derivation")
struct TierProgressTests {
    private func session(
        ordinal: Int,
        tier: UploadTier,
        state: UploadSessionState = .uploading,
        offset: UInt64,
        size: UInt64
    ) -> UploadSession {
        UploadSession(
            id: UploadID("upload-\(ordinal)"),
            assetID: "asset-\(ordinal)",
            blobRole: tier == .original ? .original : .derivative,
            tier: tier,
            state: state,
            offset: offset,
            declaredSize: size,
            ciphertextHash: BlobHash("hash-\(ordinal)")
        )
    }

    @Test("always returns all three rungs, in ladder order, even with no sessions")
    func alwaysThreeRungs() {
        let progress = TierProgress.derive(from: [])

        #expect(progress.map(\.tier) == UploadTier.ladder)
        #expect(progress.allSatisfy { $0.standing == .idle })
    }

    @Test("an idle rung reads zero, never one")
    func idleRungIsNotComplete() {
        let progress = TierProgress.derive(from: [
            session(ordinal: 0, tier: .index, offset: 100, size: 100),
        ])

        let original = progress.first { $0.tier == .original }
        #expect(original?.fractionComplete == 0)
        #expect(original?.standing == .idle)
    }

    @Test("aggregates bytes across every session on a rung")
    func aggregatesPerTier() {
        let progress = TierProgress.derive(from: [
            session(ordinal: 0, tier: .preview, offset: 50, size: 100),
            session(ordinal: 1, tier: .preview, offset: 150, size: 300),
        ])

        let preview = progress.first { $0.tier == .preview }
        #expect(preview?.transferredBytes == 200)
        #expect(preview?.totalBytes == 400)
        #expect(preview?.fractionComplete == 0.5)
        #expect(preview?.sessionCount == 2)
        #expect(preview?.standing == .inFlight)
    }

    @Test("a rung whose sessions are all terminal is settled, not in flight")
    func settledRung() {
        let progress = TierProgress.derive(from: [
            session(ordinal: 0, tier: .index, state: .completed, offset: 10, size: 10),
            session(ordinal: 1, tier: .index, state: .failedProcessing, offset: 4, size: 10),
        ])

        #expect(progress.first { $0.tier == .index }?.standing == .settled)
    }

    @Test("an offset past the declared size cannot report more than 100%")
    func clampsOverrun() {
        let progress = TierProgress.derive(from: [
            session(ordinal: 0, tier: .original, offset: 900, size: 500),
        ])

        let original = progress.first { $0.tier == .original }
        #expect(original?.transferredBytes == 500)
        #expect(original?.fractionComplete == 1)
    }

    // MARK: Tier gates

    @Test("T0 escapes on any usable link, including constrained and adverse")
    func indexEscapesOnAnyUsableLink() {
        for connection in [ConnectionClass.unmetered, .metered, .constrained, .adverse] {
            #expect(UploadTier.index.canOpen(on: connection))
        }
        #expect(!UploadTier.index.canOpen(on: .offline))
    }

    @Test("T2 waits for unmetered Wi-Fi unless the user forces it")
    func originalWaitsForWiFi() {
        #expect(!UploadTier.original.canOpen(on: .metered))
        #expect(UploadTier.original.canOpen(on: .metered, forceSync: true))
        #expect(UploadTier.original.canOpen(on: .unmetered))
    }
}

// MARK: - Throughput

@Suite("Observed throughput")
struct ThroughputSamplerTests {
    private func instant(_ seconds: Int64) -> CapsuleTimestamp {
        CapsuleTimestamp(epochSeconds: 1787400000 + seconds)
    }

    @Test("reports nothing until two samples exist")
    func needsTwoSamples() {
        var sampler = ThroughputSampler()
        sampler.record(offset: 0, at: instant(0))

        #expect(sampler.bytesPerSecond == nil)
    }

    @Test("derives a rate from the offset delta")
    func derivesRate() {
        var sampler = ThroughputSampler()
        sampler.record(offset: 0, at: instant(0))
        sampler.record(offset: 4000, at: instant(4))

        #expect(sampler.bytesPerSecond == 1000)
    }

    @Test("a backwards offset resets rather than reporting a negative rate")
    func resetsOnRealign() {
        var sampler = ThroughputSampler()
        sampler.record(offset: 8000, at: instant(0))
        sampler.record(offset: 12000, at: instant(4))
        sampler.record(offset: 2000, at: instant(8))

        #expect(sampler.bytesPerSecond == nil)
    }

    @Test("a gap longer than the window starts fresh instead of averaging it in")
    func discardsStaleGap() {
        var sampler = ThroughputSampler()
        sampler.record(offset: 0, at: instant(0))
        sampler.record(offset: 1000, at: instant(600))

        #expect(sampler.bytesPerSecond == nil)
    }
}
