import CapsuleUI
import SwiftUI

extension View {
    /// Attach the search field in whatever way the host platform currently
    /// presents search.
    ///
    /// This used to pin an iOS navigation-bar drawer to `.always`, which is the
    /// pre-26 spelling of "always visible" and actively fights the system now:
    /// Search is a `Tab(role: .search)` in the compact shell, and on 26 that
    /// role owns its own presentation — a glass field the tab bar morphs into.
    /// Forcing a drawer put a second, differently-styled field under the
    /// navigation bar of a screen the system was already giving one to.
    ///
    /// So iOS takes the automatic placement and lets the field minimize until it
    /// is reached for. macOS keeps the window-toolbar field, which is always
    /// visible there anyway and has no minimized state to opt into.
    func capsuleSearchable(
        text: Binding<String>,
        prompt: LocalizedStringKey
    ) -> some View {
        searchable(text: text, prompt: prompt)
            .capsuleSearchToolbarBehavior()
    }
}
