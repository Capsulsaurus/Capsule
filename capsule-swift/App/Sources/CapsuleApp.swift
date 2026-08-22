import CapsuleFoundation
import SwiftUI

/// The Capsule application entry point, shared by iPhone, iPad, and Mac.
///
/// One `App`, three shells. `RootView` picks the shell from the size class, so
/// the only thing that varies here is what the *platform* adds around it: macOS
/// gets a `Settings` scene, because ⌘, is where Mac users look for preferences
/// and a tab would be wrong. Everything else — the composition root, diagnostics
/// startup, the window content, the menu commands — is declared once.
@main
struct CapsuleApp: App {
    private let environment = AppEnvironment()

    init() {
        CapsuleLog.app.info("Capsule launching")
        let diagnostics = environment.diagnostics
        Task { await diagnostics.start() }
    }

    var body: some Scene {
        WindowGroup {
            RootView(environment: environment)
        }
        // A window opening at an iPhone's intrinsic size would be unusable for a
        // photo grid. Ignored where the platform owns window sizing.
        .defaultSize(width: 1280, height: 860)
        // Commands are not Mac-only: on iPad they become the ⌘-key shortcuts the
        // hardware-keyboard discovery overlay lists.
        .commands { CapsuleCommands() }

        #if os(macOS)
            settingsScene
        #endif
    }

    #if os(macOS)
        /// Preferences behind ⌘, — the same `SettingsView` the iOS tab shows,
        /// hosted where each platform's users look for it. This is why
        /// `SettingsView` takes its dependencies by constructor rather than
        /// reading them from the tab shell.
        private var settingsScene: some Scene {
            Settings {
                SettingsView(
                    consentStore: environment.consentStore,
                    diagnostics: environment.diagnostics
                )
                .frame(minWidth: 520, minHeight: 420)
            }
        }
    #endif
}
