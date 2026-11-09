import SwiftUI

// A tap reaches the app through UIKit on iOS and AppKit on macOS, so the view that observes
// it - and therefore the representable - is written twice. Both halves do the same thing: once
// the view is in a window, report every release in that window to one `InputToFrameTracer`,
// without consuming, delaying or cancelling the event. What they observe is the input itself,
// so it covers every control in the scene rather than the ones that remembered to report.

#if os(iOS)

    // The subclass header is what makes `state` settable; importing it imports UIKit.
    import UIKit.UIGestureRecognizerSubclass

    /// Marks every tap in this view's window as an `inputToFrame` interval.
    struct InputToFrameProbe: UIViewRepresentable {
        func makeUIView(context _: Context) -> ProbeView { ProbeView() }

        func updateUIView(_: ProbeView, context _: Context) {}

        final class ProbeView: UIView {
            private let recognizer: ReleaseRecognizer

            override init(frame: CGRect) {
                let frames = DisplayLinkFrames { target, selector in
                    CADisplayLink(target: target, selector: selector)
                }
                let tracer = InputToFrameTracer(postFrame: { callback in frames.post(callback) })
                recognizer = ReleaseRecognizer { tracer.onTap() }
                super.init(frame: frame)
                isUserInteractionEnabled = false
                isAccessibilityElement = false
            }

            @available(*, unavailable)
            required init?(coder _: NSCoder) {
                fatalError("InputToFrameProbe.ProbeView is created in code only")
            }

            override func didMoveToWindow() {
                super.didMoveToWindow()
                recognizer.view?.removeGestureRecognizer(recognizer)
                window?.addGestureRecognizer(recognizer)
            }
        }

        /// Reports the moment the last finger lifts, and never recognizes: it stays out of every
        /// other recognizer's way and lets touches reach their views undelayed.
        final class ReleaseRecognizer: UIGestureRecognizer, UIGestureRecognizerDelegate {
            private let onRelease: @MainActor () -> Void

            init(onRelease: @escaping @MainActor () -> Void) {
                self.onRelease = onRelease
                super.init(target: nil, action: nil)
                cancelsTouchesInView = false
                delaysTouchesBegan = false
                delaysTouchesEnded = false
                delegate = self
            }

            override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent) {
                super.touchesEnded(touches, with: event)
                let held = event.allTouches?.contains { $0.phase != .ended && $0.phase != .cancelled } ?? false
                guard !held else { return }
                onRelease()
                state = .failed
            }

            override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent) {
                super.touchesCancelled(touches, with: event)
                state = .failed
            }

            func gestureRecognizer(
                _: UIGestureRecognizer,
                shouldRecognizeSimultaneouslyWith _: UIGestureRecognizer
            ) -> Bool {
                true
            }
        }
    }

#else

    import AppKit

    /// Marks every click in this view's window as an `inputToFrame` interval.
    struct InputToFrameProbe: NSViewRepresentable {
        func makeNSView(context _: Context) -> ProbeView { ProbeView() }

        func updateNSView(_: ProbeView, context _: Context) {}

        final class ProbeView: NSView {
            private lazy var tracer: InputToFrameTracer = {
                let frames = DisplayLinkFrames { [unowned view = self] target, selector in
                    view.displayLink(target: target, selector: selector)
                }
                return InputToFrameTracer(postFrame: { callback in frames.post(callback) })
            }()

            private var monitor: Any?

            override func hitTest(_: NSPoint) -> NSView? { nil }

            override func viewDidMoveToWindow() {
                super.viewDidMoveToWindow()
                if let monitor {
                    NSEvent.removeMonitor(monitor)
                    self.monitor = nil
                }
                guard window != nil else { return }
                // A local monitor sees the event before it is dispatched and passes it on
                // unchanged; only releases in this view's own window count.
                monitor = NSEvent.addLocalMonitorForEvents(matching: .leftMouseUp) { [weak self] event in
                    MainActor.assumeIsolated {
                        if let self, event.window === self.window {
                            self.tracer.onTap()
                        }
                    }
                    return event
                }
            }
        }
    }

#endif
