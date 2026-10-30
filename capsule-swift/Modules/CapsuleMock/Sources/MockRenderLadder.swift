import CapsuleDomain

// MARK: - MockRenderLadder

/// The mock lane's ``RenderLadder``: a stand-in for `capsule_sdk`'s
/// `render_step` in the one lane that has no Rust core to call.
///
/// The ladder's order is computed once, in Rust, and the FFI lane reaches it
/// through the export (`CapsuleCatalogFFI`'s `RenderLadder.ffi`). The mock
/// lane builds no `.ffi/` directory and links no xcframework, so it would have
/// no answer at all; this is that answer, kept to the rules *Download — Render
/// Ladder* states so a mock-lane screen behaves as the FFI lane's does. It is a
/// test double, never the app's ladder: nothing outside the mock lane's
/// composition may build one, and its tests pin it to the cases the Rust
/// ladder's own tests assert.
public enum MockRenderLadder {
    /// The Thumbnail tier's long edge, which an embedded preview must reach to
    /// fill that rung (`capsule_core::media::DerivativeTier::Thumbnail`).
    public static let thumbnailLongEdge: UInt32 = 256

    /// The stand-in ladder.
    public static let ladder = RenderLadder { request, shown, holdings, fetchable in
        MockRenderLadder.step(request, shown: shown, holdings: holdings, fetchable: fetchable)
    }

    /// One step, as `render_step` answers it.
    static func step(
        _ request: RenderRequest,
        shown: Rendition?,
        holdings: RenderHoldings,
        fetchable: Set<RepresentationTier>
    ) -> RenderStep {
        let best = bestHeld(request, holdings: holdings)
        let show = shown.map { fidelity($0) >= fidelity(best) ? $0 : best } ?? best
        return RenderStep(
            show: show,
            changed: shown != show,
            next: nextStep(request, show: show, holdings: holdings, fetchable: fetchable)
        )
    }

    /// The best rendition held at or below the intent.
    private static func bestHeld(_ request: RenderRequest, holdings: RenderHoldings) -> Rendition {
        let intent = rank(request.intent)
        var candidates: [Rendition] = [.lqip]
        for tier in [RepresentationTier.thumbnail, .preview, .original]
            where rank(tier) <= intent && holdings.held.contains(tier) {
            candidates.append(Rendition(rung: tier, source: tier == .original ? .original : .encoded))
        }
        if let size = holdings.embeddedPreview {
            let covers = size.width >= request.surface.width || size.height >= request.surface.height
            if rank(.preview) <= intent, covers {
                candidates.append(Rendition(rung: .preview, source: .embedded))
            } else if rank(.thumbnail) <= intent, max(size.width, size.height) >= thumbnailLongEdge {
                candidates.append(Rendition(rung: .thumbnail, source: .embedded))
            }
        }
        return candidates.max { fidelity($0) < fidelity($1) } ?? .lqip
    }

    private static func nextStep(
        _ request: RenderRequest,
        show: Rendition,
        holdings: RenderHoldings,
        fetchable: Set<RepresentationTier>
    ) -> RenderNextStep? {
        let intent = request.intent
        guard intent != .lqip, rank(show.rung) <= rank(intent) else { return nil }
        let filled = show.rung == intent
        if filled, show.source != .embedded { return nil }
        if fetchable.contains(intent) { return .fetch(intent) }
        let generatable = intent != .original && holdings.held.contains(.original) && !filled
        return generatable ? .generate(intent) : nil
    }

    private static func rank(_ tier: RepresentationTier) -> Int {
        tier.rawValue
    }

    /// By rung, and within a rung the source above the one standing in for it.
    private static func fidelity(_ rendition: Rendition) -> Int {
        let source = switch rendition.source {
        case .lqip: 0
        case .embedded: 1
        case .encoded: 2
        case .original: 3
        }
        return rank(rendition.rung) * 4 + source
    }
}
