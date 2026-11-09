import CapsuleDomain
import CapsuleMock
import Testing

/// The mock lane's stand-in agrees with `render_step` on the cases
/// `capsule-sdk/src/fetch/ladder/tests.rs` asserts, so a screen behaves the
/// same on both lanes.
@Suite("The mock lane's render ladder answers as render_step does")
struct MockRenderLadderTests {
    private static let phone = Dimensions(width: 1170, height: 2532)
    private let ladder = MockRenderLadder.ladder

    private func request(_ intent: RepresentationTier) -> RenderRequest {
        RenderRequest(intent: intent, surface: Self.phone)
    }

    @Test("with nothing held the LQIP shows and the intent is fetched")
    func nothingHeld() {
        let step = ladder.step(request(.thumbnail), shown: nil, holdings: RenderHoldings(), fetchable: [.thumbnail])
        #expect(step.show == .lqip)
        #expect(step.changed)
        #expect(step.next == .fetch(.thumbnail))
    }

    @Test("a surface is shown the best rung held at or below its intent")
    func bestAtOrBelowIntent() {
        let holdings = RenderHoldings(held: [.thumbnail, .preview, .original])
        #expect(ladder.step(request(.thumbnail), shown: nil, holdings: holdings).show
            == Rendition(rung: .thumbnail, source: .encoded))
        #expect(ladder.step(request(.preview), shown: nil, holdings: holdings).show
            == Rendition(rung: .preview, source: .encoded))
        #expect(ladder.step(request(.original), shown: nil, holdings: holdings).show
            == Rendition(rung: .original, source: .original))
    }

    @Test("an embedded preview fills Thumbnail once its long edge reaches the tier's")
    func embeddedFillsThumbnail() {
        let reaching = RenderHoldings(held: [.original], embeddedPreview: Dimensions(width: 256, height: 170))
        let step = ladder.step(request(.thumbnail), shown: nil, holdings: reaching)
        #expect(step.show == Rendition(rung: .thumbnail, source: .embedded))
        #expect(step.next == nil)

        let short = RenderHoldings(held: [.original], embeddedPreview: Dimensions(width: 255, height: 170))
        let fallen = ladder.step(request(.thumbnail), shown: nil, holdings: short)
        #expect(fallen.show == .lqip)
        #expect(fallen.next == .generate(.thumbnail))
    }

    @Test("an embedded preview fills Preview only when it covers the surface")
    func embeddedFillsPreviewWhenItCovers() {
        let covering = RenderHoldings(held: [.original], embeddedPreview: Dimensions(width: 1170, height: 780))
        #expect(ladder.step(request(.preview), shown: nil, holdings: covering).show
            == Rendition(rung: .preview, source: .embedded))

        let small = RenderHoldings(held: [.original], embeddedPreview: Dimensions(width: 1000, height: 666))
        let step = ladder.step(request(.preview), shown: nil, holdings: small)
        #expect(step.show == Rendition(rung: .thumbnail, source: .embedded))
        #expect(step.next == .generate(.preview))
    }

    @Test("an embedded preview at the intent asks for the encoded tier where it is listed")
    func embeddedAsksForTheEncodedTier() {
        let covering = RenderHoldings(held: [.original], embeddedPreview: Dimensions(width: 1170, height: 780))
        #expect(ladder.step(request(.preview), shown: nil, holdings: covering, fetchable: [.preview]).next
            == .fetch(.preview))
        #expect(ladder.step(request(.preview), shown: nil, holdings: covering).next == nil)
    }

    @Test("a shown rung is never downgraded, and an arriving one upgrades in place")
    func upgradesInPlaceAndNeverDowngrades() {
        let shown = Rendition(rung: .preview, source: .encoded)
        let kept = ladder.step(request(.preview), shown: shown, holdings: RenderHoldings(held: [.thumbnail]))
        #expect(kept.show == shown)
        #expect(!kept.changed)

        let embedded = Rendition(rung: .preview, source: .embedded)
        let upgraded = ladder.step(request(.preview), shown: embedded, holdings: RenderHoldings(held: [.preview]))
        #expect(upgraded.show == shown)
        #expect(upgraded.changed)
        #expect(upgraded.next == nil)
    }

    @Test("a rung above the intent asks for nothing")
    func aboveIntentAsksForNothing() {
        let shown = Rendition(rung: .preview, source: .embedded)
        let step = ladder.step(request(.thumbnail), shown: shown, holdings: RenderHoldings(), fetchable: [.thumbnail])
        #expect(step.show == shown)
        #expect(step.next == nil)
    }

    @Test("the original is fetched only when it is the intent, never generated")
    func originalIsOnlyFetched() {
        #expect(ladder.step(request(.original), shown: nil, holdings: RenderHoldings(), fetchable: [.original]).next
            == .fetch(.original))
        #expect(ladder.step(request(.original), shown: nil, holdings: RenderHoldings()).next == nil)
    }

    @Test("the best in hand is the highest tier held, else the LQIP")
    func bestAvailable() {
        #expect(ladder.bestAvailable(LocalRepresentations(heldTiers: [.lqip, .thumbnail])) == .thumbnail)
        #expect(ladder.bestAvailable(LocalRepresentations(heldTiers: [.lqip])) == .lqip)
        #expect(ladder.bestAvailable(LocalRepresentations()) == .lqip)
    }
}
