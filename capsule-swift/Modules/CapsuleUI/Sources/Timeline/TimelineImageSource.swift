import AssetKit
import CapsuleDomain
import CapsuleFoundation
import CoreGraphics
import Foundation
import ImagePipeline

// MARK: - TimelineImageSource

/// The pixels behind a timeline tile, one rung of the degrade ladder at a time.
///
/// Split from `ThumbnailProvider` for two reasons. It speaks ``LibraryAsset``,
/// which is what the timeline actually holds, rather than `AssetKit.Asset`; and
/// it names the **LQIP rung** explicitly, so a tile can ask for the cheap
/// placeholder and the expensive thumbnail as two separate requests that resolve
/// in whichever order they can. A single `thumbnail(for:)` call cannot express
/// "show me something now, and something better when you have it".
///
/// - Note: ``RepresentationTier/dominantColour`` is deliberately **not** here.
///   It rides inside ``LibraryAsset/lqip`` and needs no decode, no cache, and no
///   `await` — a tile paints it synchronously in its first layout pass, which is
///   exactly why the grid is never blank.
public protocol TimelineImageSource: Sendable {
    /// The decoded LQIP placeholder, or `nil` when this build cannot decode the
    /// asset's LQIP format version.
    ///
    /// Returning `nil` is ordinary, not a failure: the ladder simply stays on
    /// the dominant colour until the thumbnail lands.
    func lqipImage(for asset: LibraryAsset) async -> PlatformImage?

    /// A grid thumbnail decoded to fill `pixelSize` device pixels.
    func thumbnail(for asset: LibraryAsset, pixelSize: CGSize) async -> PlatformImage?

    /// Warm the cache for assets about to scroll on screen.
    func beginPrefetching(_ assets: [LibraryAsset], pixelSize: CGSize) async

    /// Drop the warm-up for assets that scrolled away unseen.
    func cancelPrefetching(_ assets: [LibraryAsset], pixelSize: CGSize) async
}

public extension TimelineImageSource {
    /// Decoding an LQIP is optional: the format is owned by the image pipeline,
    /// and a source that does not own it degrades to the dominant colour rather
    /// than guessing at the bytes.
    func lqipImage(for _: LibraryAsset) async -> PlatformImage? { nil }
}

// MARK: - ThumbnailProvider adapter

/// Drives a timeline from the app's existing ``ThumbnailProvider`` — in
/// production, `ImagePipeline`.
///
/// Prefetch rides straight through to `beginPrefetching` / `cancelPrefetching`,
/// so the collection view's own prefetch and **cancel** callbacks reach
/// `PHCachingImageManager` unchanged. The cancel half is the one that matters:
/// a fast fling asks for hundreds of thumbnails the user never sees, and a
/// pipeline that is only ever told to start caching spends the whole scroll
/// decoding images that are already gone.
public struct ThumbnailProviderImageSource: TimelineImageSource {
    private let provider: any ThumbnailProvider

    public init(_ provider: any ThumbnailProvider) {
        self.provider = provider
    }

    public func thumbnail(for asset: LibraryAsset, pixelSize: CGSize) async -> PlatformImage? {
        await provider.thumbnail(for: asset.gridAsset, pixelSize: pixelSize)
    }

    public func beginPrefetching(_ assets: [LibraryAsset], pixelSize: CGSize) async {
        await provider.beginPrefetching(for: assets.map(\.gridAsset), pixelSize: pixelSize)
    }

    public func cancelPrefetching(_ assets: [LibraryAsset], pixelSize: CGSize) async {
        await provider.cancelPrefetching(for: assets.map(\.gridAsset), pixelSize: pixelSize)
    }
}

// MARK: - Projection

public extension LibraryAsset {
    /// The `AssetKit.Asset` view of this row, for the image pipeline.
    ///
    /// A projection rather than a shared type: ``LibraryAsset`` is the timeline's
    /// own already-resolved shape and carries CRDT-derived state the image
    /// pipeline has no use for, while `Asset` is what routes a request back to
    /// the provider that owns the bytes. The identifier is the same value in
    /// both, which is the only part that has to agree.
    var gridAsset: Asset {
        Asset(
            id: id,
            mediaType: mediaType,
            captureDate: Date(timeIntervalSince1970: TimeInterval(effectiveCaptureTimestamp.epochSeconds)),
            pixelWidth: Int(dimensions?.width ?? 0),
            pixelHeight: Int(dimensions?.height ?? 0),
            duration: TimeInterval(durationMilliseconds ?? 0) / 1000,
            isFavorite: rating > 0
        )
    }
}
