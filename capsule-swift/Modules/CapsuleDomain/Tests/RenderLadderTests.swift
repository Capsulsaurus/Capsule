import Foundation
import Testing

import CapsuleDomain

/// The render ladder's seam: a surface's question reaches the function it was
/// built over unchanged, and the answer comes back unchanged.
@Suite("The render ladder is one call over the function it is built from")
struct RenderLadderTests {
    /// The arguments of one call.
    private struct Call {
        var request: RenderRequest
        var shown: Rendition?
        var holdings: RenderHoldings
        var fetchable: Set<RepresentationTier>
    }

    /// Records the arguments of the calls it answers.
    private final class Recorder: @unchecked Sendable {
        private let lock = NSLock()
        private var recorded: [Call] = []

        var calls: [Call] {
            lock.withLock { recorded }
        }

        func record(_ call: Call) {
            lock.withLock { recorded.append(call) }
        }
    }

    @Test("a step passes the request, what is shown, the holdings and the fetchable set through")
    func stepForwards() throws {
        let recorder = Recorder()
        let answer = RenderStep(
            show: Rendition(rung: .preview, source: .embedded),
            changed: true,
            next: .fetch(.preview)
        )
        let ladder = RenderLadder { request, shown, holdings, fetchable in
            recorder.record(Call(request: request, shown: shown, holdings: holdings, fetchable: fetchable))
            return answer
        }
        let request = RenderRequest(intent: .preview, surface: Dimensions(width: 1170, height: 2532))
        let holdings = RenderHoldings(held: [.original], embeddedPreview: Dimensions(width: 1616, height: 1080))

        let step = ladder.step(request, shown: .lqip, holdings: holdings, fetchable: [.preview])

        #expect(step == answer)
        let call = try #require(recorder.calls.first)
        #expect(recorder.calls.count == 1)
        #expect(call.request == request)
        #expect(call.shown == .lqip)
        #expect(call.holdings == holdings)
        #expect(call.fetchable == [.preview])
    }

    @Test("the best in hand is the ladder's answer for a full-fidelity intent that can fetch nothing")
    func bestAvailableAsksForTheOriginal() throws {
        let recorder = Recorder()
        let ladder = RenderLadder { request, shown, holdings, fetchable in
            recorder.record(Call(request: request, shown: shown, holdings: holdings, fetchable: fetchable))
            return RenderStep(show: Rendition(rung: .thumbnail, source: .encoded), changed: true, next: nil)
        }

        let best = ladder.bestAvailable(LocalRepresentations(heldTiers: [.lqip, .thumbnail]))

        #expect(best == .thumbnail)
        let call = try #require(recorder.calls.first)
        #expect(call.request.intent == .original)
        #expect(call.shown == nil)
        #expect(call.holdings == RenderHoldings(held: [.thumbnail]))
        #expect(call.fetchable.isEmpty)
    }

    @Test("an embedded preview never prints its bytes")
    func embeddedPreviewDescriptionOmitsBytes() {
        let preview = EmbeddedPreview(
            jpeg: Data([0xFF, 0xD8, 0xFF, 0xD9]),
            size: Dimensions(width: 160, height: 120),
            orientation: 6
        )
        #expect(preview.description == "EmbeddedPreview(bytes: 4, size: 160x120, orientation: 6)")
    }
}
