import Foundation

/// Where to send a user who has denied Capsule access to their photo library.
///
/// Both values are plain URL schemes rather than `UIApplication.openSettingsURLString`
/// so this module stays free of UIKit; the iOS string is that constant's
/// documented value. Once `CapsuleFoundation`'s platform shim owns an
/// `appSettingsURL`, this type collapses into a call to it.
enum PhotoLibrarySettings {
    /// The settings destination that lets the user restore library access, or
    /// `nil` on a platform with no such destination.
    static var url: URL? {
        #if os(iOS)
            // Opens Capsule's own pane in Settings, where Photos access lives.
            return URL(string: "app-settings:")
        #else
            // macOS has no per-app settings pane; the Photos privacy list in
            // System Settings is the equivalent surface.
            return URL(string: "x-apple.systempreferences:com.apple.preference.security?Privacy_Photos")
        #endif
    }
}
