import CapsuleDomain
import CapsuleFoundation
import CapsuleNavigation
import FeatureAlbums
import SwiftUI

extension RouteDestination {
    /// One album's grid.
    ///
    /// `AlbumDetailView` takes the album summary rather than its id — it renders
    /// the title and count in its own chrome — while the route carries only the
    /// id, so the summary is read back through the provider on appearance. That
    /// is the whole point of an id-only payload: a restored route shows the
    /// album as it is now, not as it was when the stack was persisted.
    @ViewBuilder
    func albumDetailDestination(_ id: AlbumID) -> some View {
        let albumProvider = environment.albumProvider
        ResolvedDestination(
            titleKey: SidebarItem.albums.titleKey,
            systemImage: SidebarItem.albums.systemImage,
            resolve: { await albumProvider.loadAlbums().first { $0.id == id } },
            content: { summary in
                AlbumDetailView(
                    album: summary,
                    albumProvider: environment.albumProvider,
                    assetProvider: environment.assetProvider,
                    thumbnails: environment.thumbnails,
                    mediaLoader: environment.mediaLoader,
                    captionStore: environment.captionStore,
                    placeNames: environment.placeNames
                )
            }
        )
    }
}
