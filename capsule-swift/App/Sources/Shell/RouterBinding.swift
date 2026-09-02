import CapsuleNavigation
import SwiftUI

extension Router {
    /// A `NavigationStack`-compatible binding to one section's path.
    ///
    /// The binding lives here rather than on `Router` because
    /// `CapsuleNavigation` deliberately imports no SwiftUI — that is what lets
    /// the router be driven and asserted on in tests with no view hierarchy at
    /// all. `Binding` is the app layer's concern, so the adapter is too.
    ///
    /// Two-way by necessity: the stack must report its own pops (a swipe back,
    /// a navigation-bar tap) so the router does not drift from what is on
    /// screen.
    func binding(for item: SidebarItem) -> Binding<[Route]> {
        Binding(
            get: { self[section: item] },
            set: { self[section: item] = $0 }
        )
    }
}
