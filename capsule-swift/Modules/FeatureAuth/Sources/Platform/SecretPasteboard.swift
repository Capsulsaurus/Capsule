#if canImport(UIKit)
    import UIKit
#elseif canImport(AppKit)
    import AppKit
#endif

import Foundation

// MARK: - SecretPasteboard

/// Puts a secret on the clipboard, as carefully as each platform allows.
///
/// The user asking to copy their recovery phrase is a legitimate request — a
/// password manager is exactly where it should end up — so refusing would push
/// them towards a photograph of the screen, which is worse. What this type does
/// is make the copy as short-lived and as un-recorded as the platform permits:
///
/// - **iOS**: `localOnly` keeps it off Universal Clipboard, so the phrase does
///   not appear on the user's other devices or in another device's clipboard
///   history; an expiry clears it after two minutes.
/// - **macOS**: `NSPasteboard` has no expiry, so the honest mitigation is the
///   `org.nspasteboard.ConcealedType` marker that clipboard managers honour to
///   avoid recording a secret. The doc comment says so rather than implying a
///   guarantee AppKit does not make.
///
/// This lives in `Platform/` because it is one of the audited islands where
/// UIKit and AppKit may be imported; every other file in the module is SwiftUI
/// written once.
public enum SecretPasteboard {
    /// How long a copied secret survives where the platform can expire it.
    public static let lifetime: TimeInterval = 120

    /// The type clipboard managers read as "do not record this".
    private static let concealedType = "org.nspasteboard.ConcealedType"
    private static let plainTextType = "public.utf8-plain-text"

    /// Copy a secret. Nothing is logged: not the value, not its length, not the
    /// fact that this particular secret was the one copied.
    public static func copy(_ secret: String) {
        #if canImport(UIKit)
            UIPasteboard.general.setItems(
                [[plainTextType: secret]],
                options: [
                    .localOnly: true,
                    .expirationDate: Date().addingTimeInterval(lifetime),
                ]
            )
        #elseif canImport(AppKit)
            let concealed = NSPasteboard.PasteboardType(concealedType)
            let pasteboard = NSPasteboard.general
            // `declareTypes` is not optional here. `setString(_:forType:)` writes
            // nothing and returns `false` for a type the pasteboard has not been
            // told to expect, and a custom type is never implied — so without
            // this the concealed marker, which is the entire macOS mitigation,
            // silently fails to land while the secret itself is still written.
            // Both types are declared in one call because `declareTypes` clears
            // the pasteboard, so a second call would drop the first payload.
            pasteboard.declareTypes([concealed, .string], owner: nil)
            pasteboard.setString("", forType: concealed)
            pasteboard.setString(secret, forType: .string)
        #endif
    }
}
