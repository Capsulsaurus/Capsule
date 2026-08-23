import AssetKit
import AVFoundation
import CapsuleFoundation
import CoreGraphics
import ImageIO
import Photos

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
    /// Where a full image comes from when the asset is not a PhotoKit one.
    ///
    /// Every method below reaches PhotoKit through `phAsset(for:)`, which
    /// answers `nil` for anything that is not a `.photoKit` identifier — so a
    /// managed or mocked asset produced *no image at all* and the viewer sat on
    /// its spinner forever. That is not a mock-lane quirk: the same is true of
    /// any asset in the on-disk managed store, which is where every imported
    /// photo lives.
    ///
    /// A thumbnail provider rather than a second decode path, because the
    /// provider is already the thing that knows how to render an asset the
    /// library owns, and asking it for viewer-sized pixels is the same question
    /// the grid asks with a smaller number.
    private let fallback: (any ThumbnailProvider)?

    /// - Parameter fallback: consulted when PhotoKit has nothing. Optional so
    ///   the PhotoKit-only lane can leave it out and get the old behaviour.
    public init(fallback: (any ThumbnailProvider)? = nil) {
        self.fallback = fallback
    }

    /// A display-resolution image for `asset`, decoded to `targetSize` pixels.
    ///
    /// PhotoKit declares `requestImage` once per platform — the handler receives
    /// a `UIImage` on iOS and an `NSImage` on macOS — so the result is already a
    /// ``PlatformImage`` on both and needs no conversion here.
    public func fullImage(for asset: Asset, targetSize: CGSize) async -> PlatformImage? {
        guard let phAsset = phAsset(for: asset) else {
            return await fallback?.thumbnail(for: asset, pixelSize: targetSize)
        }
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
