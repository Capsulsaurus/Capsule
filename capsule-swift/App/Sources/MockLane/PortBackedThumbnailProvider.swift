import AssetKit
import CapsuleFoundation
import CapsuleMock
import CoreGraphics
import Foundation
import ImagePipeline

/// The grid's ``ThumbnailProvider``, over the mock's procedural renderer.
///
/// **No image bytes ship in the repository**, so the mock lane's tiles are
/// painted at request time from the asset's own derivation — the same dominant
/// colour the LQIP reports, so a placeholder and the loaded tile agree instead
/// of flashing. The renderer already caches under a byte budget, which is why
/// there is no second cache here.
///
/// It lives in the app target rather than in `AssetKit/MockBridge` for a
/// structural reason: ``ThumbnailProvider`` is declared in `ImagePipeline`,
/// and `ImagePipeline` already depends on `AssetKit`. Putting the conformance
/// in `AssetKit` would make the two frameworks mutually dependent. The app
/// target is the lowest point in the graph that can see both.
struct PortBackedThumbnailProvider: ThumbnailProvider {
    /// The smallest tile worth painting. Below this the gradient and its
    /// horizon band are indistinguishable from a flat fill.
    static let minimumEdge = 64
    /// The largest tile worth painting. A grid asks in *device* pixels, so a
    /// 3× phone requests far more than a thumbnail cache should hold; the
    /// viewer is the screen that wants full fidelity, and it does not come
    /// through here.
    static let maximumEdge = 512
    /// How many assets one prefetch pass warms. A scroll that outruns this
    /// simply paints on demand, which is cheap.
    static let prefetchBatchLimit = 64

    private let renderer: MockThumbnailRenderer

    init(renderer: MockThumbnailRenderer) {
        self.renderer = renderer
    }

    /// A painted tile, or `nil` for an identifier this library does not derive.
    ///
    /// `nil` is the honest answer for a purged asset or for one of the
    /// snapshot's provisional rows: the tile shows its absence rather than a
    /// plausible photograph standing in for one that is not there.
    func thumbnail(for asset: Asset, pixelSize: CGSize) async -> PlatformImage? {
        await renderer.thumbnail(for: asset.id, edge: Self.edge(for: pixelSize))?.makePlatformImage()
    }

    /// Warm the renderer's cache for tiles about to scroll on screen.
    func beginPrefetching(for assets: [Asset], pixelSize: CGSize) async {
        let edge = Self.edge(for: pixelSize)
        for asset in assets.prefix(Self.prefetchBatchLimit) {
            _ = await renderer.thumbnail(for: asset.id, edge: edge)
        }
    }

    /// A deliberate no-op.
    ///
    /// There is no request queue to cancel: a render happens synchronously
    /// inside the renderer's actor and is either already done or never started.
    /// Evicting what a prefetch just painted would only guarantee repainting it
    /// when the scroll reverses.
    func cancelPrefetching(for _: [Asset], pixelSize _: CGSize) async {}

    /// Drop every painted tile — what a memory-pressure notification triggers.
    func flushCaches() async {
        CapsuleLog.imagePipeline.notice("memory pressure — evicting mock thumbnails")
        await renderer.evictAll()
    }

    /// Clamp a device-pixel request to the band worth painting, on the tile's
    /// longest edge — the renderer derives the other from the asset's own
    /// aspect ratio.
    private static func edge(for pixelSize: CGSize) -> Int {
        let longest = Int(max(pixelSize.width, pixelSize.height).rounded())
        return min(maximumEdge, max(minimumEdge, longest))
    }
}
