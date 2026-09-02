#if canImport(UIKit)
    import UIKit
#elseif canImport(AppKit)
    import AppKit
#endif

import Darwin
import Foundation

/// Facts about the running platform that the UI genuinely has to branch on.
///
/// The rule this type exists to enforce: feature code branches on *capability*,
/// never on `#if os(...)`. A screen asks "does this platform have a tab bar?"
/// rather than "am I on iOS?", so a future destination (visionOS, say) is a
/// matter of extending this file rather than auditing every view.
public enum PlatformEnvironment {
    /// Whether the platform presents a compact, touch-first shell by default.
    ///
    /// Note this is about the *platform*, not the current size class — an iPad
    /// in Slide Over is still touch-first. Views that need the live size class
    /// read `\.horizontalSizeClass` instead.
    public static var isTouchFirst: Bool {
        #if os(iOS)
            return true
        #else
            return false
        #endif
    }

    /// Whether the platform has a menu bar, so commands and keyboard shortcuts
    /// have somewhere to live.
    public static var hasMenuBar: Bool {
        #if os(macOS)
            return true
        #else
            return false
        #endif
    }

    /// Whether the app can open additional windows for the same scene.
    ///
    /// `@MainActor` because the iOS answer comes from `UIDevice`, which is
    /// main-actor isolated. Every caller is UI code, so this costs nothing.
    @MainActor
    public static var supportsMultipleWindows: Bool {
        #if os(macOS)
            return true
        #else
            return UIDevice.current.userInterfaceIdiom == .pad
        #endif
    }

    /// Whether the platform's file system is user-visible, which changes how
    /// honestly the at-rest posture can be described in Settings.
    ///
    /// On iOS and iPadOS the library sits in an app-private container the
    /// sandbox denies to other apps. On macOS it is an ordinary user-directory
    /// library that any process running as the user can read — full-disk
    /// encryption is the at-rest story there, and the UI says so rather than
    /// implying a guarantee the platform does not make.
    public static var libraryIsSandboxPrivate: Bool {
        #if os(macOS)
            return false
        #else
            return true
        #endif
    }

    /// A stable, human-readable platform tag, matching the closed `PlatformTag`
    /// enum the device-cohort hash is domain-separated by.
    public static var platformTag: String {
        #if os(macOS)
            return "macos"
        #else
            return "ios"
        #endif
    }

    // MARK: Host identity

    /// The OS name, e.g. `"iOS"` or `"macOS"`.
    ///
    /// Deliberately sourced from `ProcessInfo` rather than `UIDevice` so there is
    /// one implementation for both platforms. Note this reports `"iOS"` on iPad,
    /// not `"iPadOS"` — ``hardwareModel`` is what distinguishes the two, and it
    /// does so more precisely.
    public static var systemName: String {
        #if os(macOS)
            return "macOS"
        #else
            return "iOS"
        #endif
    }

    /// The OS version as `major.minor.patch`.
    public static var systemVersion: String {
        let version = ProcessInfo.processInfo.operatingSystemVersion
        return "\(version.majorVersion).\(version.minorVersion).\(version.patchVersion)"
    }

    /// The hardware model identifier, e.g. `"iPhone17,1"` or `"Mac16,7"`.
    ///
    /// This is a *product* identifier, not a per-unit one, so it carries no more
    /// fingerprinting signal than the marketing name it replaces while being far
    /// more useful for triage.
    ///
    /// The sysctl key genuinely differs by platform: macOS puts the product in
    /// `hw.model` and only the architecture in `hw.machine`; iOS is the reverse.
    public static var hardwareModel: String {
        #if os(macOS)
            let key = "hw.model"
        #else
            let key = "hw.machine"
        #endif
        var size = 0
        guard sysctlbyname(key, nil, &size, nil, 0) == 0, size > 0 else { return "unknown" }
        var buffer = [CChar](repeating: 0, count: size)
        guard sysctlbyname(key, &buffer, &size, nil, 0) == 0 else { return "unknown" }
        let bytes = buffer.prefix { $0 != 0 }.map { UInt8(bitPattern: $0) }
        return String(bytes: bytes, encoding: .utf8) ?? "unknown"
    }
}
