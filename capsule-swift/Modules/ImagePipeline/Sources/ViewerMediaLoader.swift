import AssetKit
import AVFoundation
import CapsuleFoundation
import CoreGraphics
import ImageIO
import Photos

/// How a file encodes high dynamic range, when it does.
///
/// A closed set on purpose. These are the three states the info panel can say
/// something true about; a fourth format nobody recognises should read as
/// "no HDR row" rather than as a name the reader cannot act on.
public enum HDRFormat: String, Sendable, Equatable, CaseIterable {
    case hdr10
    case dolbyVision
    case hlg
}

/// Camera, exposure, and file metadata shown in the viewer's info panel.
///
/// Read from an asset's embedded EXIF, its container, and its PhotoKit
/// resource — or, in the mock lane, derived from the asset's own seed. Every
/// field is optional because every one of them is genuinely absent for some
/// real file: a scan has no lens, a still has no frame rate, an export has no
/// original filename.
public struct AssetExifMetadata: Sendable, Equatable {
    // Camera
    public var cameraMake: String?
    public var cameraModel: String?
    public var lensModel: String?
    /// How the camera describes the lens in use — "Main Camera", "Ultra Wide".
    /// Distinct from ``lensModel``, which is the hardware part.
    public var lensName: String?
    public var isoSpeed: Int?
    public var aperture: Double?
    public var shutterSpeed: Double?
    public var focalLength: Double?

    // File
    /// The name the file had when it was captured or imported.
    public var originalFilename: String?
    /// Size on disk, in bytes.
    public var byteCount: Int64?
    /// The codec the bytes are in — "HEVC", "H.264", "HEIF", "JPEG".
    ///
    /// A display string rather than the sidecar's `content_type`: the container
    /// MIME says `video/quicktime` where the reader wants to know it is HEVC,
    /// and those are genuinely different facts about the same file.
    public var codec: String?
    /// Frames per second, for time-based media.
    public var frameRate: Double?
    /// Which HDR encoding the file uses, when it uses one.
    public var hdrFormat: HDRFormat?

    // Location
    public var latitude: Double?
    public var longitude: Double?

    public init() {}

    /// Whether any *camera* field could be read.
    ///
    /// Scoped to the camera card rather than the whole struct: a file that knows
    /// its size and codec but nothing about the lens should still show its file
    /// line, and asking "is the whole thing empty" would hide it.
    public var hasCameraDetail: Bool {
        cameraMake != nil || cameraModel != nil || lensModel != nil || lensName != nil
            || isoSpeed != nil || aperture != nil || shutterSpeed != nil || focalLength != nil
    }

    /// Whether any file-level fact could be read.
    public var hasFileDetail: Bool {
        originalFilename != nil || byteCount != nil || codec != nil
            || frameRate != nil || hdrFormat != nil
    }

    /// Whether nothing at all could be read.
    public var isEmpty: Bool {
        !hasCameraDetail && !hasFileDetail && latitude == nil && longitude == nil
    }
}

// MARK: - AssetMetadataSource

/// Where the info panel's metadata comes from when the asset is not a PhotoKit
/// one.
///
/// The same seam ``ThumbnailProvider`` is for pixels, and it exists for the same
/// reason: `ViewerMediaLoader` reaches PhotoKit through an identifier only a
/// system asset carries, so a managed or mocked photo produced an entirely empty
/// panel — the camera and location sections silently vanished, which reads as
/// "this photo has no metadata" rather than as "this lane cannot see it".
public protocol AssetMetadataSource: Sendable {
    /// Metadata for an asset this source owns, or `nil` if it does not own it.
    func metadata(for asset: Asset) async -> AssetExifMetadata?
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

    /// Where info-panel metadata comes from when PhotoKit has nothing.
    private let metadataSource: (any AssetMetadataSource)?

    /// - Parameters:
    ///   - fallback: consulted for pixels when PhotoKit has nothing. Optional so
    ///     the PhotoKit-only lane can leave it out and get the old behaviour.
    ///   - metadataSource: consulted for info-panel metadata on the same terms.
    public init(
        fallback: (any ThumbnailProvider)? = nil,
        metadataSource: (any AssetMetadataSource)? = nil
    ) {
        self.fallback = fallback
        self.metadataSource = metadataSource
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

    /// Camera, file, and location metadata for the info panel.
    public func metadata(for asset: Asset) async -> AssetExifMetadata {
        guard let phAsset = phAsset(for: asset) else {
            return await metadataSource?.metadata(for: asset) ?? AssetExifMetadata()
        }
        let location = phAsset.location
        let data = await imageData(for: phAsset)
        var metadata = Self.parseExif(from: data)
        Self.applyResourceFacts(of: phAsset, to: &metadata)
        await applyTrackFacts(of: phAsset, to: &metadata)
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

    /// The facts that live on the *resource* rather than in the pixels:
    /// what the file is called and how large it is.
    ///
    /// `PHAssetResource` is the only place PhotoKit exposes an original
    /// filename, and the byte count arrives as an undocumented-but-stable
    /// `fileSize` value on the resource — hence the guarded cast rather than a
    /// direct read.
    private nonisolated static func applyResourceFacts(
        of phAsset: PHAsset,
        to metadata: inout AssetExifMetadata
    ) {
        let resources = PHAssetResource.assetResources(for: phAsset)
        // The primary resource, not merely the first: an edited photo carries
        // its adjustment data and its original alongside the current image, and
        // the reader is being shown the current one.
        let primary = resources.first { $0.type == .photo || $0.type == .video }
            ?? resources.first
        guard let primary else { return }
        metadata.originalFilename = primary.originalFilename
        if let size = primary.value(forKey: "fileSize") as? Int64 {
            metadata.byteCount = size
        } else if let size = primary.value(forKey: "fileSize") as? Int {
            metadata.byteCount = Int64(size)
        }
    }

    /// The facts that only the decoded container knows: codec, frame rate, and
    /// whether the video is HDR.
    ///
    /// Video only. A still's codec is already implied by its resource UTI, and
    /// loading an `AVAsset` for a photograph would be a decode nobody asked for.
    private func applyTrackFacts(of phAsset: PHAsset, to metadata: inout AssetExifMetadata) async {
        guard phAsset.mediaType == .video else { return }
        guard let facts = await Self.videoFacts(for: phAsset) else { return }
        metadata.frameRate = facts.frameRate
        metadata.codec = facts.codec
        metadata.hdrFormat = facts.hdrFormat
    }

    /// What the info panel wants to know about a video's encoding.
    ///
    /// A `Sendable` value rather than the `AVAsset` itself, so nothing
    /// non-`Sendable` crosses back to the main actor.
    private struct VideoFacts: Sendable {
        var frameRate: Double?
        var codec: String?
        var hdrFormat: HDRFormat?
    }

    /// Carries one non-`Sendable` value out of a callback API.
    ///
    /// `PHImageManager` predates structured concurrency and hands its result to
    /// a completion handler; `AVAsset` is a class with no `Sendable`
    /// conformance, so resuming a continuation with one is a diagnosed race.
    ///
    /// It is not a race *here*, and the box says why rather than hiding it: the
    /// asset is constructed by PhotoKit for this one request, handed to exactly
    /// one consumer, read on one task, and dropped. The unchecked conformance
    /// covers a single hand-off, not shared mutable state.
    private struct Unchecked<Value>: @unchecked Sendable {
        let value: Value
    }

    private nonisolated static func videoFacts(for phAsset: PHAsset) async -> VideoFacts? {
        let boxed: Unchecked<AVAsset>? = await withCheckedContinuation { continuation in
            let options = PHVideoRequestOptions()
            options.deliveryMode = .fastFormat
            options.isNetworkAccessAllowed = true
            PHImageManager.default().requestAVAsset(
                forVideo: phAsset,
                options: options
            ) { asset, _, _ in
                continuation.resume(returning: asset.map(Unchecked.init))
            }
        }
        guard let asset = boxed?.value,
              let track = try? await asset.loadTracks(withMediaType: .video).first
        else { return nil }

        var facts = VideoFacts()
        if let rate = try? await track.load(.nominalFrameRate), rate > 0 {
            facts.frameRate = Double(rate)
        }
        if let descriptions = try? await track.load(.formatDescriptions),
           let description = descriptions.first {
            facts.codec = codecName(for: CMFormatDescriptionGetMediaSubType(description))
        }
        if await (try? track.hasMediaCharacteristic(.containsHDRVideo)) == true {
            facts.hdrFormat = await hdrFormat(of: asset)
        }
        return facts
    }

    /// Which HDR encoding a video carries.
    ///
    /// Dolby Vision announces itself with its own track; everything else that
    /// reports `containsHDRVideo` is treated as HDR10, which is the honest
    /// default — HLG and HDR10 are indistinguishable at this level without
    /// reading the transfer function, and naming the wrong one is worse than
    /// naming the family.
    private nonisolated static func hdrFormat(of asset: AVAsset) async -> HDRFormat {
        guard let tracks = try? await asset.loadTracks(withMediaType: .video) else { return .hdr10 }
        for track in tracks {
            guard let descriptions = try? await track.load(.formatDescriptions) else { continue }
            let isDolbyVision = descriptions.contains {
                CMFormatDescriptionGetMediaSubType($0) == kCMVideoCodecType_DolbyVisionHEVC
            }
            if isDolbyVision { return .dolbyVision }
        }
        return .hdr10
    }

    /// A reader-facing name for a four-character codec code.
    ///
    /// The set the info panel can name. Anything else answers `nil` rather than
    /// printing the raw FourCC, which would be four characters of noise where
    /// the reader expected a format.
    private nonisolated static func codecName(for subType: CMVideoCodecType) -> String? {
        switch subType {
        case kCMVideoCodecType_HEVC, kCMVideoCodecType_HEVCWithAlpha: "HEVC"
        case kCMVideoCodecType_DolbyVisionHEVC: "Dolby Vision HEVC"
        case kCMVideoCodecType_H264: "H.264"
        case kCMVideoCodecType_AppleProRes422, kCMVideoCodecType_AppleProRes422HQ,
             kCMVideoCodecType_AppleProRes422LT, kCMVideoCodecType_AppleProRes422Proxy: "ProRes 422"
        case kCMVideoCodecType_AppleProRes4444, kCMVideoCodecType_AppleProRes4444XQ: "ProRes 4444"
        case kCMVideoCodecType_JPEG: "JPEG"
        default: nil
        }
    }
}
