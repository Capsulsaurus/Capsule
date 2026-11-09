import Testing

@testable import Capsule

/// When a tap's `inputToFrame` interval opens and closes, with frames driven by hand: it opens
/// on the tap, stays open through the frame that answers it, and closes on the frame after.
@MainActor
@Suite("Input-to-frame intervals")
struct InputToFrameTracerTests {
    /// Records interval events and holds frame callbacks until `frame()` runs them.
    @MainActor
    final class Recorder {
        var events: [String] = []
        private var pending: [@MainActor () -> Void] = []
        private var nextID = 0

        func begin() -> @MainActor () -> Void {
            let id = nextID
            nextID += 1
            events.append("begin \(id)")
            return { [self] in events.append("end \(id)") }
        }

        func post(_ callback: @escaping @MainActor () -> Void) {
            pending.append(callback)
        }

        /// One display frame: every callback posted before it runs; ones they post wait.
        func frame() {
            let due = pending
            pending = []
            due.forEach { $0() }
        }
    }

    private func makeTracer(_ recorder: Recorder) -> InputToFrameTracer {
        InputToFrameTracer(begin: { recorder.begin() }, postFrame: { recorder.post($0) })
    }

    @Test("a tap's interval closes on the frame after the one that answers it")
    func closesAfterTheAnsweringFrame() {
        let recorder = Recorder()
        let tracer = makeTracer(recorder)
        tracer.onTap()
        #expect(recorder.events == ["begin 0"])
        recorder.frame()
        #expect(recorder.events == ["begin 0"])
        recorder.frame()
        #expect(recorder.events == ["begin 0", "end 0"])
        recorder.frame()
        #expect(recorder.events == ["begin 0", "end 0"])
    }

    @Test("taps in the same frame each get their own interval")
    func overlappingTapsStayApart() {
        let recorder = Recorder()
        let tracer = makeTracer(recorder)
        tracer.onTap()
        tracer.onTap()
        recorder.frame()
        recorder.frame()
        #expect(recorder.events == ["begin 0", "begin 1", "end 0", "end 1"])
    }
}
