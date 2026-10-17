import CapsuleFoundation
import Foundation

// MARK: - NavigationShell

/// Which navigation arrangement is on screen.
///
/// Two arrangements, not three platforms. iPad and Mac both present a sidebar
/// with content and detail columns; iPhone presents one stack at a time behind
/// a tab bar. Naming the *arrangement* is what lets ``Router`` implement push,
/// pop, and column routing once instead of per platform — and it is why nothing
/// in this module reads `#if os(...)`.
///
/// It is a stored, settable property on the router rather than a constant
/// because the arrangement genuinely changes at runtime: an iPad in Slide Over
/// is compact, and rotating it back is not a relaunch. The root view sets it
/// from `\.horizontalSizeClass`; ``current`` is only the pre-layout default.
public enum NavigationShell: String, Sendable, Hashable, Codable, CaseIterable {
    /// One navigation stack at a time — the iPhone tab bar.
    case stacked
    /// Sidebar plus content and detail columns — iPad and Mac.
    case split

    /// The arrangement this platform starts in, by capability.
    ///
    /// Keyed on "can this platform show more than one window at once", which is
    /// the same capability that distinguishes a device with room for three
    /// columns from one without. Deliberately a capability question rather than
    /// an OS check, per the platform rule in `PlatformEnvironment`.
    @MainActor
    public static var current: NavigationShell {
        PlatformEnvironment.supportsMultipleWindows ? .split : .stacked
    }
}
