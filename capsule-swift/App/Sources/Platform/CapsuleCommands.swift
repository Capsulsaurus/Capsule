import SwiftUI

/// The app's menu commands.
///
/// A Mac app is judged partly on whether its menus are real — whether the
/// things you can do are discoverable there and carry the shortcuts you would
/// guess. These commands are that surface: every one of them is an action the
/// UI already offers, given a home in the menu bar and a keyboard equivalent.
///
/// They are declared for every platform, not just macOS: on iPad the same
/// declarations populate the hardware-keyboard shortcut overlay, so an iPad
/// with a Magic Keyboard gets the shortcuts for free.
///
/// Commands reach the app through the focused scene's values rather than a
/// global singleton, so a command applies to the window you are actually
/// looking at once multiple windows land.
struct CapsuleCommands: Commands {
    /// The documentation site the Help menu points at.
    private static let documentationURL = "https://capsule.photos/docs"

    var body: some Commands {
        // Replace the stock "New Window" group: Capsule's File menu is about
        // getting photos in and out, not documents.
        CommandGroup(replacing: .newItem) {
            Button("ios.menu.import", systemImage: "square.and.arrow.down") {
                NotificationCenter.default.post(name: .capsuleImportRequested, object: nil)
            }
            .keyboardShortcut("i", modifiers: .command)
        }

        CommandGroup(replacing: .help) {
            if let docs = URL(string: Self.documentationURL) {
                Link("ios.menu.help", destination: docs)
            }
        }

        // The sidebar toggle Xcode's template puts here is already provided by
        // NavigationSplitView; adding our own would duplicate ⌃⌘S.
        CommandGroup(after: .toolbar) {
            Button("ios.menu.culling_review", systemImage: "checkmark.circle") {
                NotificationCenter.default.post(name: .capsuleCullingRequested, object: nil)
            }
            .keyboardShortcut("r", modifiers: [.command, .shift])
        }
    }
}

extension Notification.Name {
    /// Posted when the File ▸ Import command fires.
    ///
    /// A notification rather than a binding because the command lives outside
    /// any view hierarchy; the focused window observes it. This is the
    /// documented SwiftUI escape hatch until the router lands and commands can
    /// address `@FocusedValue(\.router)` directly.
    static let capsuleImportRequested = Notification.Name("capsule.import.requested")

    /// Posted when the View ▸ Culling Review command fires.
    static let capsuleCullingRequested = Notification.Name("capsule.culling.requested")
}
