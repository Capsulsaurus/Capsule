import FeatureAlbums
import FeatureSearch
import FeatureTimeline
import SwiftUI
import UIKit

/// The app's root tab shell: Library, Albums, and Search.
///
/// A compact `TabView` today; Phase 7 adds the `NavigationSplitView` layout for
/// regular-width (iPad) size classes.
struct RootView: View {
    let environment: AppEnvironment

    var body: some View {
        TabView {
            Tab("Library", systemImage: "photo.on.rectangle.angled") {
                TimelineRootView(
                    assetProvider: environment.assetProvider,
                    albumProvider: environment.albumProvider,
                    thumbnails: environment.thumbnails,
                    mediaLoader: environment.mediaLoader,
                    importer: environment.importer
                )
            }
            Tab("Albums", systemImage: "rectangle.stack") {
                AlbumsRootView(
                    albumProvider: environment.albumProvider,
                    assetProvider: environment.assetProvider,
                    thumbnails: environment.thumbnails,
                    mediaLoader: environment.mediaLoader
                )
            }
            Tab("Search", systemImage: "magnifyingglass", role: .search) {
                SearchRootView(
                    assetProvider: environment.assetProvider,
                    albumProvider: environment.albumProvider,
                    thumbnails: environment.thumbnails,
                    mediaLoader: environment.mediaLoader
                )
            }
        }
        .tabViewStyle(.sidebarAdaptable)
        .onReceive(
            NotificationCenter.default.publisher(for: UIApplication.didReceiveMemoryWarningNotification)
        ) { _ in
            Task { await environment.thumbnails.flushCaches() }
        }
    }
}
