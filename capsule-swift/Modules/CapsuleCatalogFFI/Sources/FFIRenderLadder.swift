import CapsuleDomain
import CapsuleFoundation
import CapsulePorts
import Foundation
import os

/// The render ladder's log. A step is TRACE, an embedded-preview read DEBUG,
/// and a read that fails INFO, because it leaves a RAW on a slower rung.
private let renderLog = Logger(subsystem: CapsuleLog.subsystem, category: "render")

// MARK: - RenderLadder over render_step

public extension RenderLadder {
    /// The ladder `capsule_sdk` computes: every step is one `render_step`
    /// call over the FFI, so the Apple client keeps no ladder of its own.
    static let ffi = RenderLadder { request, shown, holdings, fetchable in
        let step = CapsuleCatalogFFI.renderStep(
            request: FfiRenderRequest(request),
            shown: shown.map(FfiRendition.init),
            local: FfiLocalHoldings(holdings),
            fetchable: fetchable.sorted { $0.rawValue < $1.rawValue }.map(FfiRepresentation.init)
        )
        let answer = RenderStep(step)
        renderLog.trace(
            "render step intent=\(String(describing: request.intent), privacy: .public) show=\(String(describing: answer.show), privacy: .public) next=\(String(describing: answer.next), privacy: .public)"
        )
        return answer
    }
}

// MARK: - FFIEmbeddedPreviewReader

/// The Rust-backed ``EmbeddedPreviewPort``: `embedded_preview` over the
/// original's path.
public struct FFIEmbeddedPreviewReader: EmbeddedPreviewPort {
    private let read: @Sendable (String, String) throws -> FfiEmbeddedPreview?

    public init() {
        self.init { path, ext in try CapsuleCatalogFFI.embeddedPreview(originalPath: path, ext: ext) }
    }

    /// Read through `read`. The seam tests drive.
    init(read: @escaping @Sendable (String, String) throws -> FfiEmbeddedPreview?) {
        self.read = read
    }

    public func embeddedPreview(original: URL, fileExtension: String) async throws -> EmbeddedPreview? {
        let ext = fileExtension.lowercased()
        do {
            let preview = try read(original.path, ext).map(EmbeddedPreview.init)
            renderLog.debug(
                "embedded preview ext=\(ext, privacy: .public) found=\(preview != nil, privacy: .public)"
            )
            return preview
        } catch {
            renderLog.info(
                "embedded preview unreadable ext=\(ext, privacy: .public): \(String(describing: error), privacy: .public)"
            )
            throw error
        }
    }
}

// MARK: - Record conversions

extension FfiRepresentation {
    init(_ tier: RepresentationTier) {
        self = switch tier {
        case .lqip: .lqip
        case .thumbnail: .thumbnail
        case .preview: .preview
        case .original: .original
        }
    }
}

extension RepresentationTier {
    init(_ rung: FfiRepresentation) {
        self = switch rung {
        case .lqip: .lqip
        case .thumbnail: .thumbnail
        case .preview: .preview
        case .original: .original
        }
    }
}

extension FfiRungSource {
    init(_ source: RungSource) {
        self = switch source {
        case .lqip: .lqip
        case .embedded: .embedded
        case .encoded: .encoded
        case .original: .original
        }
    }
}

extension RungSource {
    init(_ source: FfiRungSource) {
        self = switch source {
        case .lqip: .lqip
        case .embedded: .embedded
        case .encoded: .encoded
        case .original: .original
        }
    }
}

extension FfiRendition {
    init(_ shown: Rendition) {
        self.init(rung: FfiRepresentation(shown.rung), source: FfiRungSource(shown.source))
    }
}

extension Rendition {
    init(_ shown: FfiRendition) {
        self.init(rung: RepresentationTier(shown.rung), source: RungSource(shown.source))
    }
}

extension FfiRenderRequest {
    init(_ request: RenderRequest) {
        self.init(
            intent: FfiRepresentation(request.intent),
            surfaceWidth: request.surface.width,
            surfaceHeight: request.surface.height
        )
    }
}

extension FfiLocalHoldings {
    init(_ holdings: RenderHoldings) {
        self.init(
            held: holdings.held.sorted { $0.rawValue < $1.rawValue }.map(FfiRepresentation.init),
            embeddedPreviewWidth: holdings.embeddedPreview?.width,
            embeddedPreviewHeight: holdings.embeddedPreview?.height
        )
    }
}

extension RenderNextStep {
    init(_ next: FfiNextStep) {
        self = switch next {
        case let .fetch(representation): .fetch(RepresentationTier(representation))
        case let .generate(representation): .generate(RepresentationTier(representation))
        }
    }
}

extension RenderStep {
    init(_ step: FfiRenderStep) {
        self.init(show: Rendition(step.show), changed: step.changed, next: step.next.map(RenderNextStep.init))
    }
}

extension EmbeddedPreview {
    init(_ preview: FfiEmbeddedPreview) {
        self.init(
            jpeg: preview.jpeg,
            size: Dimensions(width: preview.width, height: preview.height),
            orientation: preview.orientation
        )
    }
}
