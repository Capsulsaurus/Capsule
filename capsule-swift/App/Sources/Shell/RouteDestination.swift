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
/// discovered by a user. That is also why the cases with no screen yet are
/// **named individually** rather than swept up by a `default:` — a `default:`
/// would silently absorb the next case somebody adds, which is the exact failure
/// the exhaustiveness exists to prevent.
///
/// The switch dispatches; it does not build. Each destination is a small
/// property or function, most of them in `Destinations/`, so this file stays a
/// readable table of contents. Nothing here holds state: it reads ports off
/// ``AppEnvironment`` and hands them to screens, and the screens (or a
/// ``ResolvedDestination``) own whatever has to be remembered.
struct RouteDestination: View {
    let route: Route
    let environment: AppEnvironment

    // Thirty-odd branches is what "every destination in the app" costs. The
    // complexity score is the guarantee, not a smell: collapsing it would mean
    // giving up the compile error.
    @ViewBuilder
    var body: some View {
        switch route {
        // MARK: Library

        case .timeline: timelineDestination
        case .memories, .duplicates: unbuilt
        case .trash: trashDestination
        case .hidden: hiddenDestination
        case .viewer, .culling: unbuilt

        // MARK: Collections
        case .browse: BrowseIndexView()
        case .albums: albumsDestination
        case let .album(id): albumDetailDestination(id)
        case .albumMembers, .albumPolicy: unbuilt
        case .smartAlbum, .smartAlbumEditor: unbuilt
        case .people, .person: unbuilt
        case .places, .place: placesDestination
        case .search: searchDestination

        // MARK: Transfer and provenance
        case .transferCenter: transferCenterDestination
        case let .uploadDetail(id): uploadDetailDestination(id)
        case let .custodyReceipt(id): custodyReceiptDestination(id)
        case .imports: importsDestination
        case .importSession: unbuilt
        case .quarantine: quarantineDestination
        case let .quarantineItem(id): quarantineItemDestination(id)

        // MARK: Sharing
        case .shares: sharesDestination
        case .shareDetail: unbuilt
        case .drops: dropsDestination
        case let .drop(id): dropDestination(id)
        case .linkRedemption: unbuilt

        // MARK: Fleet and federation
        case .devices: devicesDestination
        case .peers: peersDestination
        case .federation: federationDestination

        // MARK: Storage and system
        case .quota: quotaDestination
        case .storage: storageDestination
        case .maintenance: maintenanceDestination
        case let .settings(section): settingsDestination(section)
        case let .onboarding(step): onboardingDestination(step)
        }
    }

    /// The stand-in for a route that navigates correctly but has no screen yet.
    ///
    /// It borrows the owning section's own name and symbol, so the gap still
    /// reads as *that place* rather than as a generic error.
    var unbuilt: some View {
        RouteScaffold(
            titleKey: route.owningSection.titleKey,
            systemImage: route.owningSection.systemImage
        )
    }
}

// MARK: - Library and collections

extension RouteDestination {
    var timelineDestination: some View {
        TimelineRootView(
            assetProvider: environment.assetProvider,
            albumProvider: environment.albumProvider,
            thumbnails: environment.thumbnails,
            mediaLoader: environment.mediaLoader,
            captionStore: environment.captionStore,
            importer: environment.importer,
            hiddenStore: environment.hiddenStore
        )
    }

    var albumsDestination: some View {
        AlbumsRootView(
            albumProvider: environment.albumProvider,
            assetProvider: environment.assetProvider,
            thumbnails: environment.thumbnails,
            mediaLoader: environment.mediaLoader
        )
    }

    /// Memories, duplicates, trash, and hidden.
    ///
    /// Soft-deleted assets inside their retention window.
    ///
    /// Routed to the screen that shows them. Until Browse existed, `.trash`
    /// resolved to an *album index* — the screen worked, and no route reached
    /// it, because the only thing that did was a parallel `UtilityCategory`
    /// vocabulary inside a different view.
    var trashDestination: some View {
        RecentlyDeletedView(trashProvider: environment.trashProvider)
    }

    /// User-hidden assets, behind the fresh-local-auth gate.
    var hiddenDestination: some View {
        HiddenView(
            assetProvider: environment.assetProvider,
            hiddenStore: environment.hiddenStore,
            thumbnails: environment.thumbnails,
            authenticator: environment.localAuthenticator
        )
    }

    var placesDestination: some View {
        PlacesMapView(
            places: environment.places,
            assetProvider: environment.assetProvider,
            albumProvider: environment.albumProvider,
            thumbnails: environment.thumbnails,
            mediaLoader: environment.mediaLoader,
            captionStore: environment.captionStore
        )
    }

    var searchDestination: some View {
        SearchRootView(
            assetProvider: environment.assetProvider,
            albumProvider: environment.albumProvider,
            thumbnails: environment.thumbnails,
            mediaLoader: environment.mediaLoader,
            captionStore: environment.captionStore
        )
    }
}
