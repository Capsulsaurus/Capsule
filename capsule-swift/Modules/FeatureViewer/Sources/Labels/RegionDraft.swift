import CapsuleDomain
import Foundation

// MARK: - RegionDraft

/// A region being drawn over an asset's preview, in fractions of the image
/// over its display orientation.
///
/// Every edit keeps the draft inside the shape contract the core validates
/// (``RegionShape/validate()``), so ``shape`` is always writable: a drag past
/// an edge stops at the edge, and a resize never shrinks below
/// ``minimumExtent`` or grows past the image. A circle's radius is a fraction
/// of the image's **shorter** side, and since the shape does not record which
/// side that is, its centre keeps the radius from every edge on both axes.
///
/// Gestures move it by any amount; the accessibility adjust actions move it by
/// ``step``, the TUI's arrow-key step, so a VoiceOver user draws exactly the
/// regions a keyboard user does.
public enum RegionDraft: Sendable, Equatable, Hashable {
    /// A circle: centre (of width across, of height down) and radius (of the
    /// shorter side).
    case circle(centerX: Double, centerY: Double, radius: Double)
    /// A rectangle: top-left corner, width and height.
    case rect(left: Double, top: Double, width: Double, height: Double)

    /// What a new draft is.
    public enum Kind: Sendable, Equatable, Hashable, CaseIterable {
        case circle
        case rect
    }

    /// How far one accessibility adjustment moves or resizes a draft.
    public static let step = 0.02
    /// The smallest radius, width or height a draft shrinks to.
    public static let minimumExtent = 0.02

    /// A new draft of `kind` in the middle of the image.
    public init(_ kind: Kind) {
        switch kind {
        case .circle: self = .circle(centerX: 0.5, centerY: 0.5, radius: 0.16)
        case .rect: self = .rect(left: 0.34, top: 0.34, width: 0.32, height: 0.32)
        }
    }

    /// Which kind this draft is.
    public var kind: Kind {
        switch self {
        case .circle: .circle
        case .rect: .rect
        }
    }

    /// Move by `(dx, dy)` fractions, keeping the whole shape inside the image.
    public func moved(dx across: Double, dy down: Double) -> Self {
        switch self {
        case let .circle(centerX, centerY, radius):
            .circle(
                centerX: Self.clamp(centerX + across, radius, 1 - radius),
                centerY: Self.clamp(centerY + down, radius, 1 - radius),
                radius: radius
            )
        case let .rect(left, top, width, height):
            .rect(
                left: Self.clamp(left + across, 0, 1 - width),
                top: Self.clamp(top + down, 0, 1 - height),
                width: width,
                height: height
            )
        }
    }

    /// Resize by `(dw, dh)` fractions, never below ``minimumExtent`` and never
    /// past the image. A circle's radius changes by the larger of the two, and
    /// grows only until it meets the nearest edge on either axis.
    public func resized(dw wider: Double, dh taller: Double) -> Self {
        switch self {
        case let .circle(centerX, centerY, radius):
            let change = abs(wider) >= abs(taller) ? wider : taller
            let room = max(min(centerX, 1 - centerX, centerY, 1 - centerY), Self.minimumExtent)
            return .circle(centerX: centerX, centerY: centerY, radius: Self.clamp(radius + change, Self.minimumExtent, room))
        case let .rect(left, top, width, height):
            return .rect(
                left: left,
                top: top,
                width: Self.clamp(width + wider, Self.minimumExtent, 1 - left),
                height: Self.clamp(height + taller, Self.minimumExtent, 1 - top)
            )
        }
    }

    /// Scale about the shape's own centre by `factor` (a pinch), within the
    /// same bounds as ``resized(dw:dh:)``.
    public func scaled(by factor: Double) -> Self {
        guard factor.isFinite, factor > 0 else { return self }
        switch self {
        case let .circle(_, _, radius):
            return resized(dw: radius * (factor - 1), dh: 0)
        case let .rect(left, top, width, height):
            let newWidth = Self.clamp(width * factor, Self.minimumExtent, 1)
            let newHeight = Self.clamp(height * factor, Self.minimumExtent, 1)
            let centerX = left + width / 2
            let centerY = top + height / 2
            return .rect(
                left: Self.clamp(centerX - newWidth / 2, 0, 1 - newWidth),
                top: Self.clamp(centerY - newHeight / 2, 0, 1 - newHeight),
                width: newWidth,
                height: newHeight
            )
        }
    }

    /// The shape to write, in the core's single precision.
    ///
    /// Two fractions that meet an edge exactly can round past `1` in `Float`,
    /// and the core refuses a shape past the edge, so the extent gives up the
    /// last representable step until the shape fits.
    public var shape: RegionShape {
        switch self {
        case let .circle(centerX, centerY, radius):
            let middleX = Float(centerX), middleY = Float(centerY)
            var fitted = Float(radius)
            while middleX < fitted || middleY < fitted || middleX + fitted > 1 || middleY + fitted > 1 {
                fitted = fitted.nextDown
            }
            return .circle(centerX: middleX, centerY: middleY, radius: fitted)
        case let .rect(left, top, width, height):
            let fit = { (start: Float, extent: Float) -> Float in
                var extent = extent
                while start + extent > 1 {
                    extent = extent.nextDown
                }
                return extent
            }
            let edgeX = Float(left), edgeY = Float(top)
            return .rect(left: edgeX, top: edgeY, width: fit(edgeX, Float(width)), height: fit(edgeY, Float(height)))
        }
    }

    private static func clamp(_ value: Double, _ low: Double, _ high: Double) -> Double {
        min(max(value, low), max(low, high))
    }
}
