import Foundation

// MARK: - Render ladder

// The render ladder's vocabulary (*Download — Render Ladder*), as structural
// mirrors of the `capsule_sdk` uniffi records `render_step` and
// `embedded_preview` speak. The order itself is not here: it is computed once,
// in Rust, and a ``RenderLadder`` is how a surface asks it. These types only
// carry the question and the answer.

/// Where the pixels of a shown rung come from. Mirrors `FfiRungSource`.
public enum RungSource: Sendable, Equatable, Hashable, CaseIterable {
    /// The LQIP, or its dominant colour where it does not decode.
    case lqip
    /// A RAW original's embedded preview, standing in for the rung's encoded
    /// tier.
    case embedded
    /// The rung's own pixels: its encoded tier, or the rung generated on this
    /// device from the original it holds.
    case encoded
    /// The original, decoded.
    case original
}

/// What a surface shows: a rung, and the source that fills it. Mirrors
/// `FfiRendition`.
///
/// A surface keeps the last one it was told to show and passes it back as
/// `shown` on its next ``RenderLadder/step(_:shown:holdings:fetchable:)``,
/// which is what lets it upgrade in place and never downgrade.
public struct Rendition: Sendable, Equatable, Hashable {
    public var rung: RepresentationTier
    public var source: RungSource

    public init(rung: RepresentationTier, source: RungSource) {
        self.rung = rung
        self.source = source
    }

    /// The bottom of the ladder, always present.
    public static let lqip = Rendition(rung: .lqip, source: .lqip)
}

/// A surface's question. Mirrors `FfiRenderRequest`.
public struct RenderRequest: Sendable, Equatable, Hashable {
    /// The rung the surface's intent needs: ``RepresentationTier/thumbnail``
    /// for the grid, ``RepresentationTier/preview`` for the single-asset view,
    /// ``RepresentationTier/original`` for a full-fidelity view.
    public var intent: RepresentationTier
    /// The surface's size in device pixels **when the view opened**. A view
    /// that later grows keeps passing this, so it keeps an embedded preview
    /// rather than dropping a rung.
    public var surface: Dimensions

    public init(intent: RepresentationTier, surface: Dimensions) {
        self.intent = intent
        self.surface = surface
    }
}

/// What this device holds for one asset, as the ladder is told it. Mirrors
/// `FfiLocalHoldings`.
public struct RenderHoldings: Sendable, Equatable, Hashable {
    /// The encoded tiers and the original held locally. The LQIP is always
    /// held and need not be listed.
    public var held: Set<RepresentationTier>
    /// The upright size of the original's embedded preview, where this device
    /// holds a RAW original that carries one which read.
    public var embeddedPreview: Dimensions?

    public init(held: Set<RepresentationTier> = [], embeddedPreview: Dimensions? = nil) {
        self.held = held
        self.embeddedPreview = embeddedPreview
    }
}

/// How to obtain the next rung. Mirrors `FfiNextStep`.
public enum RenderNextStep: Sendable, Equatable, Hashable {
    /// Fetch the representation from the server.
    case fetch(RepresentationTier)
    /// Produce the rung on this device from the original it holds.
    case generate(RepresentationTier)
}

/// The ladder's answer. Mirrors `FfiRenderStep`.
public struct RenderStep: Sendable, Equatable, Hashable {
    /// What to show now.
    public var show: Rendition
    /// Whether ``show`` differs from the `shown` passed in.
    public var changed: Bool
    /// What to obtain next, or `nil` when the intent is met or nothing can
    /// raise it.
    public var next: RenderNextStep?

    public init(show: Rendition, changed: Bool, next: RenderNextStep?) {
        self.show = show
        self.changed = changed
        self.next = next
    }
}

/// A RAW original's embedded JPEG. Mirrors `FfiEmbeddedPreview`.
///
/// Read once per asset and kept: reading it reads the whole original.
public struct EmbeddedPreview: Sendable, Equatable, Hashable {
    /// The JPEG exactly as the RAW file carries it.
    public var jpeg: Data
    /// The upright size, once ``orientation`` is applied.
    public var size: Dimensions
    /// The EXIF orientation (1...8) to apply to ``jpeg``'s stored pixels **in
    /// place of** any tag the JPEG itself carries.
    public var orientation: UInt16

    public init(jpeg: Data, size: Dimensions, orientation: UInt16) {
        self.jpeg = jpeg
        self.size = size
        self.orientation = orientation
    }
}

extension EmbeddedPreview: CustomStringConvertible {
    /// The JPEG never reaches a log line.
    public var description: String {
        "EmbeddedPreview(bytes: \(jpeg.count), size: \(size.width)x\(size.height), orientation: \(orientation))"
    }
}

// MARK: - RenderLadder

/// The render ladder, as the one call a surface makes (`render_step`).
///
/// A value over a function rather than a port, because the ladder is pure: it
/// answers from the request, what the surface shows, what the device holds and
/// what the server lists, all of which the caller passes, and it never fetches.
/// That is what lets the design system hold one without being able to reach
/// the network. The FFI lane fills it with the `capsule_sdk` export; the mock
/// lane, which has no Rust core, with `CapsuleMock`'s stand-in.
public struct RenderLadder: Sendable {
    public typealias Step = @Sendable (
        _ request: RenderRequest,
        _ shown: Rendition?,
        _ holdings: RenderHoldings,
        _ fetchable: Set<RepresentationTier>
    ) -> RenderStep

    private let compute: Step

    public init(_ compute: @escaping Step) {
        self.compute = compute
    }

    /// One step: what to show now, and what to obtain next.
    ///
    /// `shown` is what the surface shows, `nil` when it shows nothing yet or is
    /// falling back below what it showed. `fetchable` is what the server lists
    /// for the asset and this device can still fetch.
    public func step(
        _ request: RenderRequest,
        shown: Rendition?,
        holdings: RenderHoldings,
        fetchable: Set<RepresentationTier> = []
    ) -> RenderStep {
        compute(request, shown, holdings, fetchable)
    }

    /// The best representation in hand for an asset that can fetch nothing:
    /// what ``AssetSyncState/fullResolutionUnavailable(bestAvailable:)``
    /// reports. The ladder's answer for a full-fidelity intent with nothing
    /// fetchable.
    public func bestAvailable(_ held: LocalRepresentations) -> RepresentationTier {
        step(
            RenderRequest(intent: .original, surface: Dimensions(width: 0, height: 0)),
            shown: nil,
            holdings: held.renderHoldings()
        ).show.rung
    }
}
