import CapsuleDomain
import CapsuleFoundation
import CapsuleNavigation
import FeatureTransfer
import SwiftUI

extension RouteDestination {
    /// Everything in flight, in one place.
    var transferCenterDestination: some View {
        TransferCenterView(
            uploads: environment.uploads,
            sync: environment.sync,
            library: environment.library,
            storage: environment.storage
        )
    }

    /// One asset's upload progress, chunk plan, and retry history.
    func uploadDetailDestination(_ id: AssetID) -> some View {
        UploadDetailView(
            assetID: id,
            uploads: environment.uploads,
            sync: environment.sync,
            storage: environment.storage
        )
    }

    /// The signed proof the server took durable delivery of one asset.
    func custodyReceiptDestination(_ id: AssetID) -> some View {
        CustodyReceiptView(
            assetID: id,
            uploads: environment.uploads,
            storage: environment.storage
        )
    }

    /// The quarantine inventory.
    var quarantineDestination: some View {
        QuarantineInboxView(
            quarantine: environment.quarantine,
            library: environment.library,
            sync: environment.sync
        )
    }

    /// One quarantined item and its resolutions.
    ///
    /// The port offers no read-by-id — quarantine is an inventory that is walked,
    /// not a keyed store — so the item is found in the first window. An item that
    /// has since been repaired or discarded resolves to nothing, which is the
    /// honest answer rather than an error.
    @ViewBuilder
    func quarantineItemDestination(_ id: QuarantineID) -> some View {
        let quarantine = environment.quarantine
        ResolvedDestination(
            titleKey: SidebarItem.quarantine.titleKey,
            systemImage: SidebarItem.quarantine.systemImage,
            resolve: {
                let page = try? await quarantine.items(offset: 0, limit: PageRequest.defaultLimit)
                return page?.items.first { $0.id == id }
            },
            content: { item in
                QuarantineDetailView(
                    item: item,
                    quarantine: environment.quarantine,
                    sync: environment.sync
                )
            }
        )
    }

    /// Quota headroom and its projection.
    var quotaDestination: some View {
        QuotaStatusView(
            quota: environment.quota,
            storage: environment.storage,
            sync: environment.sync
        )
    }

    /// Local occupancy and what can be released.
    var storageDestination: some View {
        StorageReclamationView(
            storage: environment.storage,
            settings: environment.settings,
            sync: environment.sync
        )
    }
}
