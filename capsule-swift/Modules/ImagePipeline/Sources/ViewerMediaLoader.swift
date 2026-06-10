import AssetKit
import AVFoundation
import ImageIO
import Photos
import UIKit

/// Camera/exposure metadata shown in the viewer's info panel, read from an
/// asset's embedded EXIF and its PhotoKit location.
public struct AssetExifMetadata: Sendable, Equatable {
    public var cameraMake: String?
    public var cameraModel: String?
    public var lensModel: String?
    public var isoSpeed: Int?
    public var aperture: Double?
    public var shutterSpeed: Double?
    public var focalLength: Double?
    public var latitude: Double?
    public var longitude: Double?

    public init() {}

    /// Whether nothing could be read.
    public var isEmpty: Bool {
        cameraMake == nil && cameraModel == nil && lensModel == nil
            && isoSpeed == nil && aperture == nil && shutterSpeed == nil
            && focalLength == nil && latitude == nil && longitude == nil
    }
}

/// Loads full-fidelity media for the full-screen viewer.
///
/// Distinct from ``ImagePipeline`` (the grid's thumbnail cache): the viewer
/// shows one asset at a time, so this is a low-volume, main-actor-confined
/// loader. Confining it to the main actor lets it return PhotoKit's
/// non-`Sendable` `PHLivePhoto` / `AVPlayerItem` to viewer views directly.
@MainActor
public final class ViewerMediaLoader {
    public init() {}

    /// A display-resolution image for `asset`, decoded to `targetSize` pixels.
    public func fullImage(for asset: Asset, targetSize: CGSize) async -> UIImage? {
        guard let phAsset = phAsset(for: asset) else { return nil }
        let options = PHImageRequestOptions()
        options.deliveryMode = .highQualityFormat
        options.isNetworkAccessAllowed = true
        options.resizeMode = .exact
        return await withCheckedContinuation { continuation in
            PHImageManager.default().requestImage(
                for: phAsset,
                targetSize: targetSize,
                contentMode: .aspectFit,
                options: options
            ) { image, _ in
                continuation.resume(returning: image)
            }
        }
    }

    /// The `PHLivePhoto` for a Live Photo asset.
    public func livePhoto(for asset: Asset, targetSize: CGSize) async -> PHLivePhoto? {
        guard let phAsset = phAsset(for: asset) else { return nil }
        let options = PHLivePhotoRequestOptions()
        options.deliveryMode = .highQualityFormat
        options.isNetworkAccessAllowed = true
        return await withCheckedContinuation { continuation in
            PHImageManager.default().requestLivePhoto(
                for: phAsset,
                targetSize: targetSize,
                contentMode: .aspectFit,
                options: options
            ) { livePhoto, _ in
                continuation.resume(returning: livePhoto)
            }
        }
    }

    /// A playable `AVPlayerItem` for a video asset.
    public func playerItem(for asset: Asset) async -> AVPlayerItem? {
        guard let phAsset = phAsset(for: asset) else { return nil }
        let options = PHVideoRequestOptions()
        options.deliveryMode = .automatic
        options.isNetworkAccessAllowed = true
        return await withCheckedContinuation { continuation in
            PHImageManager.default().requestPlayerItem(
                forVideo: phAsset,
                options: options
            ) { item, _ in
                continuation.resume(returning: item)
            }
        }
    }

    /// Camera and location metadata for the info panel.
    public func metadata(for asset: Asset) async -> AssetExifMetadata {
        guard let phAsset = phAsset(for: asset) else { return AssetExifMetadata() }
        let location = phAsset.location
        let data = await imageData(for: phAsset)
        var metadata = Self.parseExif(from: data)
        metadata.latitude = location?.coordinate.latitude
        metadata.longitude = location?.coordinate.longitude
        return metadata
    }

    // MARK: Private

    private func phAsset(for asset: Asset) -> PHAsset? {
        guard case let .photoKit(localIdentifier) = asset.id else { return nil }
        return PHAsset.fetchAssets(withLocalIdentifiers: [localIdentifier], options: nil).firstObject
    }

    private func imageData(for phAsset: PHAsset) async -> Data? {
        let options = PHImageRequestOptions()
        options.deliveryMode = .highQualityFormat
        options.isNetworkAccessAllowed = true
        return await withCheckedContinuation { continuation in
            PHImageManager.default().requestImageDataAndOrientation(
                for: phAsset,
                options: options
            ) { data, _, _, _ in
                continuation.resume(returning: data)
            }
        }
    }

    /// Parse camera/exposure fields from an image file's embedded metadata —
    /// reads container properties only, never decoding the pixels.
    private nonisolated static func parseExif(from data: Data?) -> AssetExifMetadata {
        var metadata = AssetExifMetadata()
        guard let data,
              let source = CGImageSourceCreateWithData(data as CFData, nil),
              let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any]
        else {
            return metadata
        }
        if let tiff = properties[kCGImagePropertyTIFFDictionary] as? [CFString: Any] {
            metadata.cameraMake = tiff[kCGImagePropertyTIFFMake] as? String
            metadata.cameraModel = tiff[kCGImagePropertyTIFFModel] as? String
        }
        if let exif = properties[kCGImagePropertyExifDictionary] as? [CFString: Any] {
            metadata.lensModel = exif[kCGImagePropertyExifLensModel] as? String
            metadata.isoSpeed = (exif[kCGImagePropertyExifISOSpeedRatings] as? [Int])?.first
            metadata.aperture = exif[kCGImagePropertyExifFNumber] as? Double
            metadata.shutterSpeed = exif[kCGImagePropertyExifExposureTime] as? Double
            metadata.focalLength = exif[kCGImagePropertyExifFocalLength] as? Double
        }
        return metadata
    }
}
