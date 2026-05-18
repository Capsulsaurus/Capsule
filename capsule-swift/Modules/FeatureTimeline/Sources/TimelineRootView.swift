import CapsuleUI
import SwiftUI

/// The photo timeline grid — the app's primary screen.
///
/// - Note: Phase 2 replaces this placeholder with the `UICollectionView`-backed,
///   zoomable, day/month-sectioned grid.
public struct TimelineRootView: View {
    public init() {}

    public var body: some View {
        NavigationStack {
            PlaceholderView(
                title: "Library",
                systemImage: "photo.on.rectangle.angled",
                message: "The photo timeline lands in Phase 2."
            )
            .navigationTitle("Library")
        }
    }
}
