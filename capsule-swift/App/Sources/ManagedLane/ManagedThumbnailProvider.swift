import AssetKit
import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import CoreGraphics
import Foundation
import ImagePipeline
import ManagedStore

/// The grid's and the viewer's pixels for assets in the on-disk managed store,
/// drawn as the render ladder names them.
///
/// Every picker import lands there, so without this an import was a placeholder
/// tile that never opened to a photo. The managed store holds each original and
/// no encoded tier, so the ladder is told exactly that — the original, and the
/// size of the RAW's embedded preview where ``EmbeddedPreviewPort`` read one —
/// and this provider draws what it names:
///
/// - **Embedded** — the RAW's embedded JPEG, upright by the RAW's orientation
///   (``EmbeddedPreviewDecoder``). A RAW shows at once, without being developed.
/// - **Generate** — the rung produced here from the original, by decoding it
///   at the requested size (``ManagedMediaReader``). It is reported as the
///   rung's own pixels, so a surface passing it back is not asked to produce
///   it again.
/// - **LQIP** — nothing: the managed store carries no LQIP, and `nil` leaves
///   the surface on its placeholder.
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
    private let ladder: RenderLadder
    private let embeddedPreviews: EmbeddedPreviewCache?

    /// - Parameters:
    ///   - ladder: the render ladder: `RenderLadder.ffi` in the FFI lane, the
    ///     mock lane's stand-in otherwise.
    ///   - embeddedPreviews: where a RAW's embedded preview is read; `nil` in a
    ///     lane with no Rust core, which then draws every rung from the original.
    init(reader: ManagedMediaReader, ladder: RenderLadder, embeddedPreviews: (any EmbeddedPreviewPort)? = nil) {
        self.reader = reader
        self.ladder = ladder
        self.embeddedPreviews = embeddedPreviews.map { EmbeddedPreviewCache(port: $0) }
    }

    /// The grid's tile: the Thumbnail rung at the tile's size.
    func thumbnail(for asset: Asset, pixelSize: CGSize) async -> PlatformImage? {
        let request = RenderRequest(intent: .thumbnail, pixelSize: pixelSize)
        return await render(asset, request: request, shown: nil, pixelSize: pixelSize)?.image
    }

    /// The decoded rung, or `nil` for an asset the managed catalog does not
    /// index — which is what lets a composite ask this provider first.
    func render(_ asset: Asset, request: RenderRequest, shown: Rendition?, pixelSize: CGSize) async -> RenderedImage? {
        guard case let .managed(uuid) = asset.id,
              let original = await reader.mediaFile(uuid: uuid)
        else { return nil }
        let preview = await embeddedPreviews?.preview(uuid: uuid, original: original)
        let holdings = RenderHoldings(held: [.original], embeddedPreview: preview?.size)
        let step = ladder.step(request, shown: shown, holdings: holdings)
        let edge = Self.edge(for: pixelSize)
        CapsuleLog.managedStore.debug(
            "render \(uuid, privacy: .public) show=\(String(describing: step.show), privacy: .public) next=\(String(describing: step.next), privacy: .public)"
        )

        // Generate now when there is nothing better to draw first, or when the
        // surface already shows what was drawn first and asks again.
        if case let .generate(rung) = step.next, shown != nil || step.show.source == .lqip {
            return await generated(uuid: uuid, rung: rung, edge: edge)
        }
        switch step.show.source {
        case .lqip:
            return nil
        case .embedded:
            if let preview, let image = EmbeddedPreviewDecoder.image(preview, maxPixelEdge: edge) {
                return RenderedImage(image: .fromCGImage(image), rendition: step.show, next: step.next)
            }
            // A preview that does not decode leaves the rung to the next
            // source: the original.
            CapsuleLog.managedStore.info("embedded preview of \(uuid, privacy: .public) did not decode")
            return await generated(uuid: uuid, rung: step.show.rung, edge: edge)
        case .encoded, .original:
            // The managed store keeps no encoded tier: an encoded rung shown
            // here is one this provider generated, so it is generated again
            // (from the reader's cache, when it is still warm).
            guard let image = await reader.image(uuid: uuid, maxPixelEdge: edge) else { return nil }
            return RenderedImage(image: .fromCGImage(image), rendition: step.show, next: step.next)
        }
    }

    func beginPrefetching(for assets: [Asset], pixelSize: CGSize) async {
        for asset in assets.prefix(Self.prefetchBatchLimit) {
            _ = await thumbnail(for: asset, pixelSize: pixelSize)
        }
    }

    /// A deliberate no-op: a decode is either already done or never started,
    /// and evicting a fresh one would only repeat it when the scroll reverses.
    func cancelPrefetching(for _: [Asset], pixelSize _: CGSize) async {}

    func flushCaches() async {
        reader.evictAll()
        embeddedPreviews?.evictAll()
    }

    /// Clamp a device-pixel request to the band worth decoding, on the longest
    /// edge — ImageIO keeps the aspect ratio.
    static func edge(for pixelSize: CGSize) -> Int {
        let longest = Int(max(pixelSize.width, pixelSize.height).rounded())
        return min(maximumEdge, max(minimumEdge, longest))
    }

    /// The rung produced from the original, in memory.
    private func generated(uuid: String, rung: RepresentationTier, edge: Int) async -> RenderedImage? {
        guard let image = await reader.image(uuid: uuid, maxPixelEdge: edge) else { return nil }
        return RenderedImage(image: .fromCGImage(image), rendition: Rendition(rung: rung, source: .encoded))
    }
}

// MARK: - EmbeddedPreviewCache

/// Each managed asset's embedded preview, read once and kept under a byte
/// budget — reading one reads the whole original. An asset with no preview is
/// remembered too, so a JPEG is not re-read on every scroll past it.
///
/// `@unchecked Sendable` because its only mutable state is an `NSCache`, which
/// is documented thread-safe.
private final class EmbeddedPreviewCache: @unchecked Sendable {
    /// The cache's byte ceiling: a few dozen camera-sized previews.
    static let byteBudget = 48 * 1024 * 1024

    private let port: any EmbeddedPreviewPort
    private let storage = NSCache<NSString, Entry>()

    init(port: any EmbeddedPreviewPort) {
        self.port = port
        storage.totalCostLimit = Self.byteBudget
    }

    func preview(uuid: String, original: URL) async -> EmbeddedPreview? {
        if let cached = storage.object(forKey: uuid as NSString) { return cached.preview }
        let preview: EmbeddedPreview?
        do {
            preview = try await port.embeddedPreview(original: original, fileExtension: original.pathExtension)
        } catch {
            // An unreadable file is the reader's failure to report; for the
            // ladder it is only an absent preview, and is not remembered.
            CapsuleLog.managedStore.info(
                "embedded preview read failed for \(uuid, privacy: .public): \(String(describing: error), privacy: .public)"
            )
            return nil
        }
        storage.setObject(Entry(preview), forKey: uuid as NSString, cost: (preview?.jpeg.count ?? 0) + 64)
        return preview
    }

    func evictAll() {
        storage.removeAllObjects()
    }

    private final class Entry {
        let preview: EmbeddedPreview?

        init(_ preview: EmbeddedPreview?) {
            self.preview = preview
        }
    }
}
