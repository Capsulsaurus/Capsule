import CapsuleUI
import SwiftUI

/// The search screen — find photos by date, media type, and place.
///
/// - Note: implemented in Phase 6.
public struct SearchRootView: View {
    public init() {}

    public var body: some View {
        NavigationStack {
            PlaceholderView(
                title: "Search",
                systemImage: "magnifyingglass",
                message: "Search & filters land in Phase 6."
            )
            .navigationTitle("Search")
        }
    }
}
