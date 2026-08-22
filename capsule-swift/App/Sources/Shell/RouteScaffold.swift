import CapsuleNavigation
import SwiftUI

/// The stand-in for a destination that routes correctly but has no interface
/// yet.
///
/// It exists so navigation can be complete before the screens are. Every
/// `Route` resolves to *something*, which means the sidebar, the deep links,
/// the menu commands, and the UI tests can all be exercised end to end while
/// individual screens are still being built — and a route that was never wired
/// up shows as an obvious gap rather than a dead tap.
///
/// It is deliberately plain. A convincing mock here would hide the gap, and the
/// point is to make it visible.
struct RouteScaffold: View {
    /// The catalog key for the destination's own name.
    let titleKey: String
    /// The SF Symbol the section carries elsewhere in the app, so the scaffold
    /// still reads as that place.
    let systemImage: String

    var body: some View {
        ContentUnavailableView {
            Label(LocalizedStringKey(titleKey), systemImage: systemImage)
        } description: {
            Text("ios.scaffold.body")
        }
        .navigationTitle(LocalizedStringKey(titleKey))
    }
}

#Preview("Scaffold") {
    NavigationStack {
        RouteScaffold(titleKey: "ios.sidebar.quarantine", systemImage: "exclamationmark.shield")
    }
}
