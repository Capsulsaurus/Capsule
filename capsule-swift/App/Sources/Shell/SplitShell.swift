import CapsuleNavigation
import CapsuleUI
import SwiftUI

/// The iPad and Mac shell: a sidebar of every section beside a navigation
/// stack.
///
/// Two columns rather than three. A third (detail) column is right for a
/// mail-shaped app, but a photo grid wants the width: the viewer is a
/// full-bleed presentation, not a pane. Routes that want the detail treatment
/// are still marked as such by `Route.preferredColumn`, and the viewer reads it
/// to decide between an inspector and a sheet.
struct SplitShell: View {
    let environment: AppEnvironment
    @Bindable var router: Router

    var body: some View {
        NavigationSplitView(columnVisibility: sidebarVisibility) {
            List(selection: sidebarSelection) {
                ForEach(SidebarGroup.allCases, id: \.self) { group in
                    Section(LocalizedStringKey(group.titleKey)) {
                        ForEach(SidebarItem.sections(in: group)) { item in
                            Label(
                                LocalizedStringKey(item.titleKey),
                                systemImage: item.systemImage
                            )
                            .tag(item)
                        }
                    }
                }
            }
            .navigationTitle("app_name")
            #if os(macOS)
                .navigationSplitViewColumnWidth(min: 200, ideal: 240, max: 320)
            #endif
        } detail: {
            NavigationStack(path: router.binding(for: router.selection)) {
                RouteDestination(route: router.selection.rootRoute, environment: environment)
                    .navigationDestination(for: Route.self) { route in
                        RouteDestination(route: route, environment: environment)
                    }
            }
        }
        .navigationSplitViewStyle(.balanced)
    }

    /// The sidebar's selection, as an optional.
    ///
    /// `List(selection:)` takes a `Binding<SelectionValue?>` on iOS and iPadOS —
    /// the non-optional overload is macOS-only — so the portable spelling is an
    /// optional binding. The router's selection is never actually absent, so a
    /// `nil` write (which SwiftUI sends when a row is deselected) is ignored
    /// rather than forcing an artificial "no section" state into the router.
    private var sidebarSelection: Binding<SidebarItem?> {
        Binding(
            get: { router.selection },
            set: { newValue in
                guard let newValue else { return }
                router.selection = newValue
            }
        )
    }

    /// Bridges the router's sidebar flag to SwiftUI's column visibility.
    ///
    /// The flag lives on the router because ⌃⌘S fires from the menu bar, which
    /// is outside any view hierarchy and so cannot reach view state directly.
    private var sidebarVisibility: Binding<NavigationSplitViewVisibility> {
        Binding(
            get: { router.isSidebarVisible ? .all : .detailOnly },
            set: { router.isSidebarVisible = $0 != .detailOnly }
        )
    }
}
