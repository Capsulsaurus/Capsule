import AssetKit
import CapsuleFoundation
import CoreGraphics
import Foundation
import ImagePipeline
import ManagedStore

/// The grid's and the viewer's pixels for assets in the on-disk managed store.
///
/// Every picker import lands there, so without this an import was a placeholder
/// tile that never opened to a photo. The decoding and caching live in
/// ``ManagedMediaReader``; this is the projection onto ``ThumbnailProvider``.
///
/// It lives in the app target for the reason ``PortBackedThumbnailProvider``
/// does: ``ThumbnailProvider`` is declared in `ImagePipeline`, which depends on
/// `AssetKit` and therefore on `ManagedStore`, so neither of those can conform
/// to it. The app is the lowest point in the graph that sees both sides.
struct ManagedThumbnailProvider: ThumbnailProvider {
    /// The smallest edge worth decoding.
    static let minimumEdge = 64
    /// The largest edge decoded. Higher than the mock renderer's because the
    /// viewer comes through here too and wants a full-screen frame; above this a
    /// phone screen shows no more detail.
    static let maximumEdge = 4096
    /// How many assets one prefetch pass warms.
    static let prefetchBatchLimit = 64

    private let reader: ManagedMediaReader

    init(reader: ManagedMediaReader) {
        self.reader = reader
    }

    /// The decoded asset, or `nil` for one the managed catalog does not index —
    /// which is what lets a composite ask this provider first.
    func thumbnail(for asset: Asset, pixelSize: CGSize) async -> PlatformImage? {
        guard case let .managed(uuid) = asset.id else { return nil }
        guard let image = await reader.image(uuid: uuid, maxPixelEdge: Self.edge(for: pixelSize)) else {
            return nil
        }
        return PlatformImage.fromCGImage(image)
    }

    func beginPrefetching(for assets: [Asset], pixelSize: CGSize) async {
        let edge = Self.edge(for: pixelSize)
        for asset in assets.prefix(Self.prefetchBatchLimit) {
            guard case let .managed(uuid) = asset.id else { continue }
            _ = await reader.image(uuid: uuid, maxPixelEdge: edge)
        }
    }

    /// A deliberate no-op: a decode is either already done or never started,
    /// and evicting a fresh one would only repeat it when the scroll reverses.
    func cancelPrefetching(for _: [Asset], pixelSize _: CGSize) async {}

    func flushCaches() async {
        reader.evictAll()
    }

    /// Clamp a device-pixel request to the band worth decoding, on the longest
    /// edge — ImageIO keeps the aspect ratio.
    static func edge(for pixelSize: CGSize) -> Int {
        let longest = Int(max(pixelSize.width, pixelSize.height).rounded())
        return min(maximumEdge, max(minimumEdge, longest))
    }
}
