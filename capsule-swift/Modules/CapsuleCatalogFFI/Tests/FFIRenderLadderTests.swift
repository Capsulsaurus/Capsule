import CapsuleDomain
import CapsulePorts
import Foundation
import Testing

@testable import CapsuleCatalogFFI

/// The Apple client's render ladder is `render_step`, reached over the FFI:
/// these run the real export, not a double.
@Suite("RenderLadder.ffi answers through render_step")
struct FFIRenderLadderTests {
    private static let phone = Dimensions(width: 1170, height: 2532)

    @Test("with nothing held the LQIP shows and the listed intent is fetched")
    func nothingHeld() {
        let step = RenderLadder.ffi.step(
            RenderRequest(intent: .thumbnail, surface: Self.phone),
            shown: nil,
            holdings: RenderHoldings(),
            fetchable: [.thumbnail]
        )
        #expect(step.show == .lqip)
        #expect(step.changed)
        #expect(step.next == .fetch(.thumbnail))
    }

    @Test("a covering embedded preview fills Preview on the device holding the original")
    func embeddedFillsPreview() {
        let step = RenderLadder.ffi.step(
            RenderRequest(intent: .preview, surface: Self.phone),
            shown: nil,
            holdings: RenderHoldings(held: [.original], embeddedPreview: Dimensions(width: 1170, height: 780))
        )
        #expect(step.show == Rendition(rung: .preview, source: .embedded))
        #expect(step.next == nil)
    }

    @Test("a shown rung passed back is kept, and an arriving one upgrades it in place")
    func upgradesInPlace() {
        let request = RenderRequest(intent: .preview, surface: Self.phone)
        let embedded = Rendition(rung: .preview, source: .embedded)

        let kept = RenderLadder.ffi.step(request, shown: embedded, holdings: RenderHoldings())
        #expect(kept.show == embedded)
        #expect(!kept.changed)

        let upgraded = RenderLadder.ffi.step(request, shown: embedded, holdings: RenderHoldings(held: [.preview]))
        #expect(upgraded.show == Rendition(rung: .preview, source: .encoded))
        #expect(upgraded.changed)
    }

    @Test("a held original generates the intent's rung when nothing is listed")
    func generatesFromTheOriginal() {
        let step = RenderLadder.ffi.step(
            RenderRequest(intent: .thumbnail, surface: Self.phone),
            shown: nil,
            holdings: RenderHoldings(held: [.original])
        )
        #expect(step.show == .lqip)
        #expect(step.next == .generate(.thumbnail))
    }

    @Test("every tier and source survives the round trip through its record")
    func recordsRoundTrip() {
        for tier in RepresentationTier.allCases {
            #expect(RepresentationTier(FfiRepresentation(tier)) == tier)
        }
        for source in RungSource.allCases {
            #expect(RungSource(FfiRungSource(source)) == source)
        }
        let holdings = FfiLocalHoldings(
            RenderHoldings(held: [.original, .thumbnail], embeddedPreview: Dimensions(width: 640, height: 480))
        )
        #expect(holdings.held == [.thumbnail, .original])
        #expect(holdings.embeddedPreviewWidth == 640)
        #expect(holdings.embeddedPreviewHeight == 480)
    }

    @Test("the reader passes the path and the lowercased extension, and maps the record")
    func readerMapsTheRecord() async throws {
        let jpeg = Data([0xFF, 0xD8, 0xFF, 0xD9])
        let reader = FFIEmbeddedPreviewReader { path, ext in
            #expect(path == "/library/media/2026/2026-01/a.ARW")
            #expect(ext == "arw")
            return FfiEmbeddedPreview(jpeg: jpeg, width: 1616, height: 1080, orientation: 6)
        }

        let preview = try await reader.embeddedPreview(
            original: URL(fileURLWithPath: "/library/media/2026/2026-01/a.ARW"),
            fileExtension: "ARW"
        )

        #expect(preview == EmbeddedPreview(jpeg: jpeg, size: Dimensions(width: 1616, height: 1080), orientation: 6))
    }

    @Test("a still with no embedded preview reads as none, not as a failure")
    func noPreviewIsNone() async throws {
        let reader = FFIEmbeddedPreviewReader { _, _ in nil }
        let preview = try await reader.embeddedPreview(original: URL(fileURLWithPath: "/a.jpg"), fileExtension: "jpg")
        #expect(preview == nil)
    }
}
