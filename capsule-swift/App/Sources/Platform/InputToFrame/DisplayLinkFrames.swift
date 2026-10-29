import QuartzCore

/// Runs callbacks on the next display frame, through a display link that exists only while a
/// callback is pending, so an idle app keeps no link firing.
///
/// A callback posted from inside a frame's callbacks runs on the frame after, which is what
/// `InputToFrameTracer`'s two hops rely on.
@MainActor
final class DisplayLinkFrames: NSObject {
    /// Makes a display link calling `target`'s `selector`; the platform decides how
    /// (`CADisplayLink(target:selector:)` on iOS, `NSView.displayLink(target:selector:)` on
    /// macOS).
    typealias MakeLink = @MainActor (_ target: Any, _ selector: Selector) -> CADisplayLink

    private let makeLink: MakeLink
    private var pending: [@MainActor () -> Void] = []
    private var link: CADisplayLink?

    init(makeLink: @escaping MakeLink) {
        self.makeLink = makeLink
    }

    /// Run `callback` on the next frame.
    func post(_ callback: @escaping @MainActor () -> Void) {
        pending.append(callback)
        guard link == nil else { return }
        let created = makeLink(self, #selector(frame))
        created.add(to: .main, forMode: .common)
        link = created
    }

    @objc private func frame() {
        let due = pending
        pending = []
        due.forEach { $0() }
        // A callback above may have posted the next hop; keep the link for it.
        if pending.isEmpty {
            link?.invalidate()
            link = nil
        }
    }
}
