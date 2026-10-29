import Foundation

// MARK: - RegionGesture

/// One drag-and-pinch interaction on a ``RegionDraft``, composed from a
/// single base.
///
/// A drag and a pinch run simultaneously, and each reports its cumulative
/// change since it began. Both changes are applied to the draft as it was when
/// the first of them began, so neither erases the other while both run. A
/// gesture that ends folds its final change into the settled total, so the
/// other one carries on from the same base without compounding, and a gesture
/// that begins again while the other still runs adds to what the first one
/// left.
public struct RegionGesture: Sendable, Equatable {
    /// The draft as it was when the interaction began.
    public let base: RegionDraft
    private var settledDX = 0.0
    private var settledDY = 0.0
    private var settledMagnification = 1.0
    private var dragDX = 0.0
    private var dragDY = 0.0
    private var magnification = 1.0
    private var isDragging = false
    private var isPinching = false

    /// An interaction starting from `base`.
    public init(base: RegionDraft) {
        self.base = base
    }

    /// Whether a drag or a pinch is still running.
    public var isActive: Bool {
        isDragging || isPinching
    }

    /// The draft with both changes applied to ``base``.
    public var draft: RegionDraft {
        base
            .scaled(by: settledMagnification * magnification)
            .moved(dx: settledDX + dragDX, dy: settledDY + dragDY)
    }

    /// The drag's cumulative translation, in fractions of the image.
    public mutating func drag(dx: Double, dy: Double) {
        isDragging = true
        dragDX = dx
        dragDY = dy
    }

    /// The drag ended; its translation stays applied.
    public mutating func endDrag() {
        settledDX += dragDX
        settledDY += dragDY
        dragDX = 0
        dragDY = 0
        isDragging = false
    }

    /// The pinch's cumulative magnification.
    public mutating func pinch(by factor: Double) {
        guard factor.isFinite, factor > 0 else { return }
        isPinching = true
        magnification = factor
    }

    /// The pinch ended; its magnification stays applied.
    public mutating func endPinch() {
        settledMagnification *= magnification
        magnification = 1
        isPinching = false
    }
}
