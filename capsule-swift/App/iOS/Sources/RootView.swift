import FeatureAlbums
import FeatureSearch
import FeatureTimeline
import SwiftUI

/// The app's root tab shell: Library, Albums, and Search.
///
/// A compact `TabView` today; Phase 7 adds the `NavigationSplitView` layout for
/// regular-width (iPad) size classes.
struct RootView: View {
    var body: some View {
        TabView {
            Tab("Library", systemImage: "photo.on.rectangle.angled") {
                TimelineRootView()
            }
            Tab("Albums", systemImage: "rectangle.stack") {
                AlbumsRootView()
            }
            Tab("Search", systemImage: "magnifyingglass", role: .search) {
                SearchRootView()
            }
        }
    }
}

#Preview {
    RootView()
}
