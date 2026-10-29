import CapsuleFoundation
import OSLog

/// Marks each tap's input-to-frame latency as one `CapsuleSignpost.inputToFrame` interval,
/// which the parity benchmark reads with an `XCTOSSignpostMetric`.
///
/// The interval opens when the tap is delivered and closes at the start of the frame *after*
/// the one that answers it. The tap's state change is committed at the end of the run-loop turn
/// that delivered it, so the first frame callback marks the frame that picks the commit up and
/// the second runs once that frame has been drawn. That is the "visible response in the next
/// frame" the responsiveness contract bounds, measured from inside the app, and it is the same
/// two-hop rule the Android client's `InputToFrameTracer` follows.
@MainActor
final class InputToFrameTracer {
    /// Opens an interval and returns what closes it.
    typealias Begin = @MainActor () -> @MainActor () -> Void
    /// Runs a callback on the next display frame.
    typealias PostFrame = @MainActor (@escaping @MainActor () -> Void) -> Void

    private let begin: Begin
    private let postFrame: PostFrame

    /// - Parameters:
    ///   - begin: opens one interval; the `inputToFrame` signpost in production.
    ///   - postFrame: schedules a callback for the next frame (a display link in production).
    init(begin: @escaping Begin = InputToFrameTracer.signpost, postFrame: @escaping PostFrame) {
        self.begin = begin
        self.postFrame = postFrame
    }

    /// A tap was delivered; call on the main thread.
    func onTap() {
        let end = begin()
        let hop = postFrame
        hop {
            hop {
                end()
            }
        }
    }

    /// One `inputToFrame` signpost interval, with its own id so overlapping taps stay apart.
    static func signpost() -> @MainActor () -> Void {
        let signposter = CapsuleSignpost.inputToFrame
        let state = signposter.beginInterval(
            CapsuleSignpost.inputToFrameInterval,
            id: signposter.makeSignpostID()
        )
        return {
            signposter.endInterval(CapsuleSignpost.inputToFrameInterval, state)
        }
    }
}
