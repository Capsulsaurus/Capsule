import SwiftUI

extension View {
    /// Attach a search field that is on screen from the moment the screen is,
    /// rather than one the user has to pull down to reveal.
    ///
    /// The iOS spelling for that is a navigation-bar drawer pinned to `.always`,
    /// a placement that exists only where there is a navigation bar. macOS puts
    /// the field in the window toolbar, which is already always visible, so the
    /// intent survives the platform change even though the spelling cannot.
    func capsuleAlwaysVisibleSearchable(
        text: Binding<String>,
        prompt: LocalizedStringKey
    ) -> some View {
        #if os(iOS)
            searchable(
                text: text,
                placement: .navigationBarDrawer(displayMode: .always),
                prompt: prompt
            )
        #else
            searchable(text: text, placement: .toolbar, prompt: prompt)
        #endif
    }
}
