import AssetKit
import AVFoundation
import CapsuleDomain
import CapsuleFoundation
import CoreGraphics
import ImageIO
import ManagedStore
import Photos

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
            // The original's own frame, never a stand-in: an export shares
            // these pixels and the region editor measures against them, and an
            // embedded preview can be cropped or scaled differently.
            let request = RenderRequest(intent: .original, pixelSize: targetSize)
            return await fallback?.render(asset, request: request, shown: nil, pixelSize: targetSize)?.image
        }
        return await photoKitImage(phAsset, targetSize: targetSize)
    }

    /// How many frames one ``loadViewerImage(for:surface:targetSize:shown:onFrame:)``
    /// delivers at most: what is held now, then the rung generated from the
    /// original. The ladder needs no more, and the bound keeps a provider that
    /// keeps asking from spinning.
    static let maximumRenderSteps = 3

    /// The single-asset view's pixels, through the render ladder.
    ///
    /// Asks for ``RepresentationTier/preview`` at `surface`, the view's size in
    /// device pixels when it opened, and hands each frame to `onFrame` as it
    /// arrives: first the best rung held — a RAW's embedded preview where it
    /// covers the surface — then, where the ladder says to generate the rung
    /// from the original this device holds, that rung, so the view upgrades in
    /// place. `shown` is the rendition the view already shows, and the view
    /// keeps the last frame's ``RenderedImage/rendition`` to pass back.
    ///
    /// A system-library asset is PhotoKit's to render and is delivered once,
    /// outside the ladder.
    public func loadViewerImage(
        for asset: Asset,
        surface: CGSize,
        targetSize: CGSize,
        shown: Rendition?,
        onFrame: (RenderedImage) -> Void
    ) async {
        if let phAsset = phAsset(for: asset) {
            if let image = await photoKitImage(phAsset, targetSize: targetSize) {
                onFrame(RenderedImage(image: image, rendition: nil))
            }
            return
        }
        guard let fallback else { return }
        let request = RenderRequest(intent: .preview, pixelSize: surface)
        var current = shown
        for _ in 0 ..< Self.maximumRenderSteps {
            let frame = await fallback.render(asset, request: request, shown: current, pixelSize: targetSize)
            guard let frame, !Task.isCancelled else { return }
            onFrame(frame)
            guard let rendition = frame.rendition, case .generate = frame.next else { return }
            current = rendition
        }
        CapsuleLog.imagePipeline.notice("viewer render stopped at its step bound")
    }

    private func photoKitImage(_ phAsset: PHAsset, targetSize: CGSize) async -> PlatformImage? {
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
    /// reads container properties only, never decoding the pixels. The parse
    /// is ``ContainerExif``'s, shared with the managed store; its GPS fix is
    /// not taken, because PhotoKit's location is the one this panel shows.
    private nonisolated static func parseExif(from data: Data?) -> AssetExifMetadata {
        var metadata = AssetExifMetadata()
        guard let data else { return metadata }
        let exif = ContainerExif.parse(data)
        metadata.cameraMake = exif.cameraMake
        metadata.cameraModel = exif.cameraModel
        metadata.lensModel = exif.lensModel
        metadata.isoSpeed = exif.isoSpeed
        metadata.aperture = exif.aperture
        metadata.shutterSpeed = exif.shutterSpeed
        metadata.focalLength = exif.focalLength
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
