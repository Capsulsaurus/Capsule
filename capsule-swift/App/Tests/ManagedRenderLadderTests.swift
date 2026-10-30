import CapsuleDomain
import CapsuleMock
import CapsulePorts
import CoreGraphics
import Foundation
import ImagePipeline
import Testing

@testable import Capsule

/// The managed store's pixels come out of the render ladder: the grid and the
/// viewer draw the rung it names, a RAW's embedded preview first where one was
/// read, and a rung the store does not hold is generated from the original.
@MainActor
@Suite("Managed lane rendering through the ladder")
struct ManagedRenderLadderTests {
    @Test("with no embedded preview the ladder's rung is generated from the original")
    func rungIsGeneratedFromTheOriginal() async throws {
        let root = ManagedLaneTests.temporaryRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let mock = MockEnvironment(scenario: .emptyLibrary)
        let lane = AppEnvironment.makeManagedLane(root: root.appending(path: "Library", directoryHint: .isDirectory))
        let thumbnails = AppEnvironment.composeThumbnails(
            library: PortBackedThumbnailProvider(renderer: mock.thumbnails),
            managed: lane
        )
        let imported = try await ManagedLaneTests.importPhoto(through: lane, in: root)
        let size = CGSize(width: 128, height: 128)

        let tile = try #require(await thumbnails.render(
            imported,
            request: RenderRequest(intent: .thumbnail, pixelSize: size),
            shown: nil,
            pixelSize: size
        ))

        #expect(tile.rendition == Rendition(rung: .thumbnail, source: .encoded))
        #expect(tile.next == nil)
    }

    @Test("a RAW's embedded preview is shown first, upright by the RAW's orientation")
    func embeddedPreviewIsShownUpright() async throws {
        let root = ManagedLaneTests.temporaryRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let mock = MockEnvironment(scenario: .emptyLibrary)
        // Stored landscape, tagged by the RAW as rotated 90° clockwise: upright
        // it is portrait, unlike the 320×240 original.
        let preview = try EmbeddedPreview(
            jpeg: ManagedLaneTests.jpeg(width: 400, height: 300),
            size: Dimensions(width: 300, height: 400),
            orientation: 6
        )
        let lane = AppEnvironment.makeManagedLane(
            root: root.appending(path: "Library", directoryHint: .isDirectory),
            embeddedPreviews: FixedEmbeddedPreview(preview: preview)
        )
        let thumbnails = AppEnvironment.composeThumbnails(
            library: PortBackedThumbnailProvider(renderer: mock.thumbnails),
            managed: lane
        )
        let imported = try await ManagedLaneTests.importPhoto(through: lane, in: root)
        let viewer = ViewerMediaLoader(fallback: thumbnails)

        var frames: [RenderedImage] = []
        await viewer.loadViewerImage(
            for: imported,
            surface: CGSize(width: 300, height: 400),
            targetSize: CGSize(width: 600, height: 800),
            shown: nil
        ) { frames.append($0) }

        let frame = try #require(frames.first)
        #expect(frames.count == 1)
        #expect(frame.rendition == Rendition(rung: .preview, source: .embedded))
        #expect(frame.image.size.height > frame.image.size.width)

        // An export and the region editor still read the original's own frame.
        let original = try #require(await viewer.fullImage(for: imported, targetSize: CGSize(width: 640, height: 640)))
        #expect(original.size.width > original.size.height)
    }

    /// An embedded-preview read that answers `preview` for every original.
    private struct FixedEmbeddedPreview: EmbeddedPreviewPort {
        let preview: EmbeddedPreview

        func embeddedPreview(original _: URL, fileExtension _: String) async throws -> EmbeddedPreview? {
            preview
        }
    }
}
