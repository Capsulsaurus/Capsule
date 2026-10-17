import CapsuleFoundation
import Combine
import Foundation

/// The stream of "the system is under memory pressure" events, as a publisher a
/// SwiftUI view can subscribe to unconditionally.
///
/// `PlatformLifecycle.memoryWarningNotification` is `nil` on macOS, which has no
/// such notification. Rather than making the call site `#if` around
/// `onReceive`, that absence is modelled as a publisher that never fires —
/// subscribing is then always valid and the handler simply never runs.
enum MemoryPressure {
    static var publisher: AnyPublisher<Notification, Never> {
        guard let name = PlatformLifecycle.memoryWarningNotification else {
            return Empty<Notification, Never>(completeImmediately: false).eraseToAnyPublisher()
        }
        return NotificationCenter.default.publisher(for: name).eraseToAnyPublisher()
    }
}
