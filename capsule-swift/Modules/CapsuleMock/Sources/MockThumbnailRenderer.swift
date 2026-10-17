import CapsuleDomain
import CapsuleFoundation
import CoreGraphics
import Foundation

// MARK: - MockThumbnailRenderer

/// Draws every thumbnail the mock library shows, at request time.
///
/// **No image bytes are in the repository.** A photo app's fixtures are the one
/// asset class that would dominate a repository's size and licensing surface, so
/// the tiles are painted instead: a gradient around the asset's dominant colour,
/// a little structural noise so the grid does not look like a paint chart, and a
/// darker band that gives each tile an unambiguous top and bottom. The colour is
/// the *same* derivation ``Lqip/dominantColor`` reports, so the placeholder and
/// the loaded tile agree instead of flashing.
///
/// Renders are cached under a byte budget. A 250 000-asset scroll would
/// otherwise paint and retain every tile it passed, and a mock that leaks memory
/// while proving the timeline does not is not proving much.
public actor MockThumbnailRenderer {
    /// The cache's byte ceiling. Around 900 thumbnails at the default size —
    /// several screens of scrollback, and small enough to be invisible next to a
    /// real decoded-image cache.
    public static let defaultByteBudget = 48 * 1024 * 1024

    private nonisolated let library: MockLibrary
    private let cache = NSCache<NSString, CachedThumbnail>()
    private var renderCount = 0

    public init(library: MockLibrary, byteBudget: Int = MockThumbnailRenderer.defaultByteBudget) {
        self.library = library
        cache.totalCostLimit = byteBudget
    }

    /// How many renders have actually been performed — a cache-effectiveness
    /// assertion for tests, and nothing a screen should read.
    public var rendersPerformed: Int { renderCount }

    /// The thumbnail for an asset at a requested edge length.
    ///
    /// Returns `nil` for an identifier this library does not derive, which is
    /// the honest answer: a tile for a purged asset should show its absence, not
    /// a plausible photograph.
    public func thumbnail(for identifier: AssetID, edge: Int = 256) -> MockThumbnail? {
        guard let ref = MockAssetRef.decode(identifier), library.contains(ref) else { return nil }
        let key = "\(ref.kind.rawValue):\(ref.index):\(ref.memberOrdinal):\(edge)" as NSString
        if let cached = cache.object(forKey: key) { return cached.value }
        guard let rendered = render(ref: ref, edge: edge) else { return nil }
        renderCount += 1
        cache.setObject(CachedThumbnail(rendered), forKey: key, cost: rendered.byteCount)
        return rendered
    }

    /// Drop everything cached — what a memory-pressure notification triggers.
    public func evictAll() {
        cache.removeAllObjects()
    }

    // MARK: Drawing

    /// Paint one tile.
    ///
    /// The aspect ratio comes from the asset's own dimensions, so a portrait
    /// photograph produces a portrait tile and a grid's aspect-fit maths has
    /// something real to work against.
    private func render(ref: MockAssetRef, edge: Int) -> MockThumbnail? {
        let derivation = ref.derivationIndex
        let seed = library.profile.seed
        let size = tileSize(ref: ref, edge: edge)
        guard let context = makeContext(width: size.width, height: size.height) else { return nil }
        let dominant = MockPalette.dominantColour(seed: seed, derivationIndex: derivation)
        let shadow = MockPalette.shadowColour(seed: seed, derivationIndex: derivation)
        paintGradient(in: context, size: size, from: dominant, to: shadow)
        paintHorizon(in: context, size: size, derivationIndex: derivation, seed: seed)
        applyNoise(to: context, size: size, derivationIndex: derivation, seed: seed)
        guard let pixels = copyPixels(from: context, size: size) else { return nil }
        return MockThumbnail(
            width: size.width,
            height: size.height,
            pixels: pixels,
            dominantColor: dominant
        )
    }

    private func tileSize(ref: MockAssetRef, edge: Int) -> (width: Int, height: Int) {
        let type = library.contentType(for: ref)
        let dimensions = library.dimensions(for: ref, contentType: type)
        guard let ratio = dimensions.aspectRatio, ratio > 0 else { return (edge, edge) }
        return ratio >= 1
            ? (edge, max(1, Int(Double(edge) / ratio)))
            : (max(1, Int(Double(edge) * ratio)), edge)
    }

    private func makeContext(width: Int, height: Int) -> CGContext? {
        CGContext(
            data: nil,
            width: width,
            height: height,
            bitsPerComponent: 8,
            bytesPerRow: width * 4,
            space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        )
    }

    private func paintGradient(
        in context: CGContext,
        size: (width: Int, height: Int),
        from start: CapsuleDomain.RGBColor,
        to end: CapsuleDomain.RGBColor
    ) {
        let colours = [start, end].map { colour in
            CGColor(
                red: CGFloat(colour.red) / 255,
                green: CGFloat(colour.green) / 255,
                blue: CGFloat(colour.blue) / 255,
                alpha: 1
            )
        }
        guard let gradient = CGGradient(
            colorsSpace: CGColorSpaceCreateDeviceRGB(),
            colors: colours as CFArray,
            locations: [0, 1]
        ) else { return }
        context.drawLinearGradient(
            gradient,
            start: CGPoint(x: 0, y: CGFloat(size.height)),
            end: CGPoint(x: CGFloat(size.width), y: 0),
            options: [.drawsBeforeStartLocation, .drawsAfterEndLocation]
        )
    }

    /// A single darker band across the frame. Enough structure that a reviewer
    /// scrolling the grid can tell one tile from another and see that tiles are
    /// not being reused, without pretending to be a photograph.
    private func paintHorizon(
        in context: CGContext,
        size: (width: Int, height: Int),
        derivationIndex: Int,
        seed: UInt64
    ) {
        let hash = MockHash.value(seed: seed, index: derivationIndex, salt: .orientation, sub: 3)
        let position = 0.18 + MockHash.fraction(hash) * 0.6
        let thickness = max(1, Int(Double(size.height) * (0.03 + MockHash.fraction(MockHash.mix(hash)) * 0.08)))
        let originY = Int(Double(size.height) * position)
        context.setFillColor(gray: 0.08, alpha: 0.26)
        context.fill(CGRect(x: 0, y: originY, width: size.width, height: thickness))
    }

    /// Deterministic per-pixel grain, written straight into the bitmap.
    ///
    /// Every eleventh pixel rather than every pixel: enough to break up the
    /// gradient's banding, cheap enough that painting a screenful of tiles
    /// during a scroll is not the thing that drops the frame.
    private func applyNoise(
        to context: CGContext,
        size: (width: Int, height: Int),
        derivationIndex: Int,
        seed: UInt64
    ) {
        guard let raw = context.data else { return }
        let total = size.width * size.height
        let buffer = raw.bindMemory(to: UInt8.self, capacity: total * 4)
        var hash = MockHash.value(seed: seed, index: derivationIndex, salt: .colour, sub: 9)
        for pixel in stride(from: 0, to: total, by: 11) {
            hash = MockHash.mix(hash)
            let delta = Int8(bitPattern: UInt8(truncatingIfNeeded: hash)) / 8
            for channel in 0 ..< 3 {
                let offset = pixel * 4 + channel
                let value = Int(buffer[offset]) + Int(delta)
                buffer[offset] = UInt8(min(255, max(0, value)))
            }
        }
    }

    private func copyPixels(from context: CGContext, size: (width: Int, height: Int)) -> Data? {
        guard let raw = context.data else { return nil }
        return Data(bytes: raw, count: size.width * size.height * 4)
    }
}

// MARK: - CachedThumbnail

/// `NSCache` holds objects, so the value type needs a box. Private, because
/// nothing outside the renderer should hold a reference to a thumbnail.
private final class CachedThumbnail {
    let value: MockThumbnail

    init(_ value: MockThumbnail) {
        self.value = value
    }
}
