import CapsuleUI
import SwiftUI

/// The albums screen — system smart albums plus Capsule-managed user albums.
///
/// - Note: implemented in Phase 5.
public struct AlbumsRootView: View {
    public init() {}

    public var body: some View {
        NavigationStack {
            PlaceholderView(
                title: "Albums",
                systemImage: "rectangle.stack",
                message: "Albums land in Phase 5."
            )
            .navigationTitle("Albums")
        }
    }
}
