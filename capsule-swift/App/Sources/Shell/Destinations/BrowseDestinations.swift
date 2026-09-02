import CapsuleNavigation
import SwiftUI

// MARK: - BrowseIndexView

/// The phone's index of every section its tab bar cannot carry.
///
/// A tab bar seats four or five items and the app has twenty sections. Rather
/// than let the phone quietly lose the other sixteen — which is exactly what it
/// did before this screen existed — one tab is spent on reaching them.
///
/// The rows are derived from ``SidebarItem/browsable(in:)`` and the headings
/// from the same `ios.sidebar.group.*` keys the iPad sidebar uses, so the two
/// surfaces cannot drift in wording or order. That is the point of the sidebar
/// catalog being data: adding a section makes it appear here and in the
/// sidebar, in the same place, without either shell being edited.
///
/// Rows are plain `NavigationLink(value:)` rather than router pushes because
/// this view is already inside the shell's `NavigationStack`, whose path is
/// `[Route]` — a link into that stack *is* the router's stack for this section.
struct BrowseIndexView: View {
    var body: some View {
        List {
            ForEach(SidebarGroup.allCases, id: \.self) { group in
                let rows = SidebarItem.browsable(in: group)
                if !rows.isEmpty {
                    Section(LocalizedStringKey(group.titleKey)) {
                        ForEach(rows) { item in
                            row(for: item)
                        }
                    }
                }
            }
        }
        .navigationTitle(LocalizedStringKey(SidebarItem.browse.titleKey))
    }

    private func row(for item: SidebarItem) -> some View {
        NavigationLink(value: item.rootRoute) {
            Label(LocalizedStringKey(item.titleKey), systemImage: item.systemImage)
        }
        .accessibilityIdentifier("browse.\(item.rawValue)")
    }
}

#Preview {
    NavigationStack {
        BrowseIndexView()
    }
}
