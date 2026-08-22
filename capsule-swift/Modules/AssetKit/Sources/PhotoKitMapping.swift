import CapsuleFoundation
import Foundation
import Photos

// Pure mappings from PhotoKit types into the source-agnostic model. Kept
// separate from `PhotoKitProvider` so the translation is small, obvious, and
// (for the authorization mapping) unit-testable without a photo library.

extension Asset {
    /// Builds a unified ``Asset`` from a system Photos `PHAsset`.
    init(photoKitAsset phAsset: PHAsset) {
        let mediaType: MediaType =
            switch phAsset.mediaType {
            case .video:
                .video
            case .image:
                phAsset.mediaSubtypes.contains(.photoLive) ? .livePhoto : .photo
            default:
                .photo
            }
        self.init(
            id: .photoKit(localIdentifier: phAsset.localIdentifier),
            mediaType: mediaType,
            captureDate: phAsset.creationDate ?? phAsset.modificationDate ?? .distantPast,
            pixelWidth: phAsset.pixelWidth,
            pixelHeight: phAsset.pixelHeight,
            duration: phAsset.duration,
            isFavorite: phAsset.isFavorite
        )
    }
}

extension AssetAuthorizationStatus {
    /// Maps a PhotoKit authorization status to the source-agnostic status.
    ///
    /// `.limited` is reachable only on iOS — macOS has no limited-library
    /// picker, so it answers full access or nothing. The case still maps
    /// unconditionally: the platform simply never produces it, which is
    /// cheaper than a branch that has to be kept true.
    init(photoKit status: PHAuthorizationStatus) {
        self =
            switch status {
            case .notDetermined: .notDetermined
            case .restricted: .restricted
            case .denied: .denied
            case .authorized: .authorized
            case .limited: .limited
            @unknown default: .denied
            }
    }
}
