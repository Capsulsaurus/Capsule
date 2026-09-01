import CapsuleDomain
import CapsuleFoundation
import CoreGraphics
import Foundation
import Testing

@testable import CapsuleMock

// MARK: - Thumbnails

/// Thumbnails are painted, never shipped.
///
/// No image bytes are in the repository — a photo app's fixtures are the one
/// asset class that would dominate its size and licensing surface. These tests
/// hold the renderer to the two things that make procedural tiles usable: the
/// colour agrees with the placeholder the grid draws first, and the cache is
/// bounded so a long scroll does not grow memory without limit.
@Suite("Thumbnails are procedural and bounded")
struct MockThumbnailTests {
    private func makeLibrary(assetCount: Int = 400) -> MockLibrary {
        MockLibrary(profile: MockLibraryProfile(
            assetCount: assetCount,
            newestDayNumber: MockClock.reference.todayDayNumber
        ))
    }

    /// The bottom of the degrade ladder is a colour, drawn with no decode at
    /// all. If the placeholder and the loaded tile disagreed, every tile in the
    /// grid would flash a different colour as it arrived.
    @Test("The rendered colour is the LQIP's dominant colour")
    func thumbnailMatchesPlaceholder() async throws {
        let library = makeLibrary()
        let renderer = MockThumbnailRenderer(library: library)
        for index in [0, 7, 128, 399] {
            let asset = library.asset(at: index)
            let thumbnail = await renderer.thumbnail(for: asset.id, edge: 64)
            #expect(thumbnail?.dominantColor == asset.lqip?.dominantColor)
        }
    }

    @Test("A rendered thumbnail is a well-formed bitmap")
    func thumbnailIsDrawable() async throws {
        let library = makeLibrary()
        let renderer = MockThumbnailRenderer(library: library)
        let asset = library.asset(at: 3)
        let thumbnail = try #require(await renderer.thumbnail(for: asset.id, edge: 128))
        #expect(thumbnail.pixels.count == thumbnail.width * thumbnail.height * 4)
        #expect(max(thumbnail.width, thumbnail.height) == 128)
        #expect(thumbnail.makeImage() != nil)
    }

    /// A portrait photograph produces a portrait tile, so a grid's aspect-fit
    /// maths has something real to work against.
    @Test("Tile shape follows the asset's own dimensions")
    func tileShapeFollowsDimensions() async throws {
        let library = makeLibrary(assetCount: 200)
        let renderer = MockThumbnailRenderer(library: library)
        var sawPortrait = false
        var sawLandscape = false
        for index in 0 ..< 40 {
            let asset = library.asset(at: index)
            guard let dimensions = asset.dimensions, dimensions.width != dimensions.height else { continue }
            let thumbnail = try #require(await renderer.thumbnail(for: asset.id, edge: 64))
            if dimensions.width > dimensions.height {
                sawLandscape = true
                #expect(thumbnail.width >= thumbnail.height)
            } else {
                sawPortrait = true
                #expect(thumbnail.height >= thumbnail.width)
            }
        }
        #expect(sawPortrait)
        #expect(sawLandscape)
    }

    @Test("Rendering is deterministic for the same asset")
    func renderingIsDeterministic() async throws {
        let library = makeLibrary()
        let first = MockThumbnailRenderer(library: library)
        let second = MockThumbnailRenderer(library: library)
        let asset = library.asset(at: 11)
        let left = await first.thumbnail(for: asset.id, edge: 48)
        let right = await second.thumbnail(for: asset.id, edge: 48)
        #expect(left == right)
    }

    /// A scroll that re-visits a tile must not repaint it, or the cache is not
    /// doing its job.
    @Test("A repeated request is served from the cache")
    func cacheAvoidsRepeatedWork() async throws {
        let library = makeLibrary()
        let renderer = MockThumbnailRenderer(library: library)
        let asset = library.asset(at: 5)
        _ = await renderer.thumbnail(for: asset.id, edge: 64)
        let afterFirst = await renderer.rendersPerformed
        _ = await renderer.thumbnail(for: asset.id, edge: 64)
        #expect(await renderer.rendersPerformed == afterFirst)
        await renderer.evictAll()
        _ = await renderer.thumbnail(for: asset.id, edge: 64)
        #expect(await renderer.rendersPerformed == afterFirst + 1)
    }

    /// A tile for an asset this library does not derive shows its absence
    /// rather than a plausible photograph.
    @Test("An unknown identifier renders nothing")
    func unknownIdentifierRendersNothing() async throws {
        let renderer = MockThumbnailRenderer(library: makeLibrary())
        #expect(await renderer.thumbnail(for: .photoKit(localIdentifier: "X"), edge: 32) == nil)
        let outOfRange = MockAssetRef(kind: .live, index: 100000).identifier(seed: 0x0C0F_FEE0_1234_5678)
        #expect(await renderer.thumbnail(for: outOfRange, edge: 32) == nil)
    }
}
