import CapsuleDomain
import CapsuleFoundation
import CapsuleNavigation
import FeatureSharing
import SwiftUI

extension RouteDestination {
    /// Share links this library has issued, live and lapsed.
    var sharesDestination: some View {
        ShareLinkListView(
            model: ShareLinkListViewModel(
                share: environment.sharing,
                connectivity: environment.sharingConnectivity
            )
        )
    }

    /// Guest uploads awaiting adoption.
    var dropsDestination: some View {
        DropInboxView(
            drops: environment.drops,
            albums: environment.albums,
            connectivity: environment.sharingConnectivity
        )
    }

    /// One pending drop.
    ///
    /// Read back from the first window for the same reason the quarantine detail
    /// is: the port exposes an inbox to walk, not a keyed store, and a drop that
    /// another device already adopted is legitimately gone.
    @ViewBuilder
    func dropDestination(_ id: DropID) -> some View {
        let drops = environment.drops
        ResolvedDestination(
            titleKey: SidebarItem.drops.titleKey,
            systemImage: SidebarItem.drops.systemImage,
            resolve: {
                let page = try? await drops.pendingDrops(offset: 0, limit: PageRequest.defaultLimit)
                return page?.items.first { $0.id == id }
            },
            content: { drop in
                DropDetailView(
                    model: DropDetailViewModel(
                        drop: drop,
                        drops: environment.drops,
                        albums: environment.albums,
                        connectivity: environment.sharingConnectivity
                    )
                )
            }
        )
    }

    /// Devices on this network that originals can be fetched from.
    var peersDestination: some View {
        PeeringView(
            model: PeeringViewModel(
                peering: environment.peering,
                connectivity: environment.sharingConnectivity
            )
        )
    }

    /// Aggregated albums across origins, and the origins that are lagging.
    var federationDestination: some View {
        FederationView(
            model: FederationViewModel(
                federation: environment.federation,
                moderation: environment.moderation,
                connectivity: environment.sharingConnectivity
            )
        )
    }
}
