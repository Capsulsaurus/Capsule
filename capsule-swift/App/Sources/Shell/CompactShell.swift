import CapsuleNavigation
import CapsuleUI
import SwiftUI

/// The iPhone shell: a floating Liquid Glass tab bar over four sections, each
/// with its own navigation stack.
///
/// A tab bar holds four or five items comfortably and the app has nineteen
/// sections, so `SidebarItem.tabs` promotes four and the rest are reached
/// through the Collections section. That split is declared once in the sidebar
/// catalog, which is what stops a section from becoming unreachable on a phone
/// when someone adds one.
///
/// Each tab binds its *own* path from the router, so switching tabs and coming
/// back restores where you were — the behaviour a tab bar implies, and the
/// reason the router keeps a stack per section rather than one global path.
struct CompactShell: View {
    let environment: AppEnvironment
    @Bindable var router: Router

    var body: some View {
        TabView(selection: $router.selection) {
            ForEach(SidebarItem.tabs) { item in
                Tab(
                    LocalizedStringKey(item.titleKey),
                    systemImage: item.systemImage,
                    value: item,
                    role: item == .search ? .search : nil
                ) {
                    NavigationStack(path: router.binding(for: item)) {
                        RouteDestination(route: item.rootRoute, environment: environment)
                            .navigationDestination(for: Route.self) { route in
                                RouteDestination(route: route, environment: environment)
                            }
                    }
                    // Names the section currently *showing*, which is how the
                    // UI sweep knows where a tap landed without reading the
                    // catalog it cannot import.
                    .accessibilityIdentifier("section.\(item.rawValue)")
                }
                // On the tab bar *item*, not on its page: the sweep has to be
                // able to select a section it is not already looking at.
                .accessibilityIdentifier("tab.\(item.rawValue)")
            }
        }
        .tabViewStyle(.sidebarAdaptable)
        .capsuleTabBarMinimizeOnScroll()
    }
}
