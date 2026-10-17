#if canImport(UIKit)
    import UIKit
#endif

import Foundation

/// Cross-platform hooks for the process lifecycle events the app reacts to.
///
/// iOS delivers a memory-pressure warning that the thumbnail caches respond to;
/// macOS has no analogue, so the notification name is `nil` there and callers
/// simply never fire. Encoding "this platform does not have that event" as an
/// optional keeps the call sites free of `#if`.
public enum PlatformLifecycle {
    /// Posted when the system is under memory pressure and the app should
    /// release what it can. `nil` on platforms with no such notification.
    public static var memoryWarningNotification: Notification.Name? {
        #if canImport(UIKit) && !os(macOS)
            return UIApplication.didReceiveMemoryWarningNotification
        #else
            return nil
        #endif
    }
}
