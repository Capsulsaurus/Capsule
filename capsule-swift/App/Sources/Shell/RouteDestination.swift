import CapsuleNavigation
import FeatureAlbums
import FeatureCollections
import FeatureSearch
import FeatureTimeline
import SwiftUI

/// Resolves a `Route` to the view that presents it.
///
/// This is the one place the navigation vocabulary meets the screens, and it is
/// deliberately a single exhaustive `switch`: adding a `Route` case without
/// giving it a destination is then a compile error rather than a dead tap
/// discovered by a user.
///
/// Destinations that are not built yet resolve to ``RouteScaffold``, which keeps
/// the section's own name and symbol. That is what lets the sidebar, deep links,
/// menu commands, and the UI-test sweep all run end to end while the individual
/// screens are still being filled in.
struct RouteDestination: View {
    let route: Route
    let environment: AppEnvironment

    var body: some View {
        switch route {
        case .timeline:
            TimelineRootView(
                assetProvider: environment.assetProvider,
                albumProvider: environment.albumProvider,
                thumbnails: environment.thumbnails,
                mediaLoader: environment.mediaLoader,
                importer: environment.importer,
                hiddenStore: environment.hiddenStore
            )

        case .albums:
            AlbumsRootView(
                albumProvider: environment.albumProvider,
                assetProvider: environment.assetProvider,
                thumbnails: environment.thumbnails,
                mediaLoader: environment.mediaLoader
            )

        // The Collections screen is the compact shell's route to everything the
        // iPhone tab bar cannot hold, so it stands in for those sections until
        // each gets its own destination.
        case .memories, .duplicates, .trash, .hidden, .imports:
            CollectionsRootView(
                albumProvider: environment.albumProvider,
                assetProvider: environment.assetProvider,
                trashProvider: environment.trashProvider,
                hiddenStore: environment.hiddenStore,
                thumbnails: environment.thumbnails,
                mediaLoader: environment.mediaLoader
            )

        case .places, .place:
            PlacesMapView(
                assetProvider: environment.assetProvider,
                albumProvider: environment.albumProvider,
                thumbnails: environment.thumbnails,
                mediaLoader: environment.mediaLoader
            )

        case .search:
            SearchRootView(
                assetProvider: environment.assetProvider,
                albumProvider: environment.albumProvider,
                thumbnails: environment.thumbnails,
                mediaLoader: environment.mediaLoader
            )

        case .settings:
            SettingsView(
                consentStore: environment.consentStore,
                diagnostics: environment.diagnostics
            )

        default:
            RouteScaffold(
                titleKey: route.owningSection.titleKey,
                systemImage: route.owningSection.systemImage
            )
        }
    }
}
