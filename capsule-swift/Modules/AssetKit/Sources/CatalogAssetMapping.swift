import CapsuleCatalog
import CapsuleFoundation
import Foundation

extension Asset {
    /// Builds a unified ``Asset`` from a managed-catalog row — shared by the
    /// managed timeline and album providers.
    init(catalogAsset row: CatalogAsset) {
        self.init(
            id: .managed(uuid: row.id),
            mediaType: row.assetType == "video" ? .video : .photo,
            captureDate: Date(timeIntervalSince1970: TimeInterval(row.effectiveCaptureTimestamp)),
            pixelWidth: Int(row.width ?? 0),
            pixelHeight: Int(row.height ?? 0),
            duration: TimeInterval(row.durationMillis ?? 0) / 1000,
            isFavorite: row.rating > 0
        )
    }
}
