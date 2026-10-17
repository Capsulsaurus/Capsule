import CoreGraphics
import Foundation

/// The geometry behind ``ZoomableImageView``.
///
/// Split out and kept pure — no SwiftUI, no platform image — because "does the
/// photo stay under your finger, and can you push it off screen?" is exactly
/// the kind of thing that should be answered by tests rather than by pinching a
/// device.
///
/// The model: the image is laid out aspect-fit in the container at zoom `1`,
/// scaled about the container's centre, then translated by an offset in
/// container points. Every helper here works in that space.
enum ZoomableImageMath {
    /// The resting scale — the image exactly aspect-fits its container.
    static let minZoom: CGFloat = 1
    /// Four times the fit scale, matching the viewer's previous `UIScrollView`.
    static let maxZoom: CGFloat = 4
    /// How much of an out-of-bounds drag survives as movement. Matches the feel
    /// of a scroll view's rubber band closely enough to be indistinguishable in
    /// the hand, without pretending to be UIKit's exact curve.
    static let rubberBandFactor: CGFloat = 0.3

    /// The size the image occupies at zoom `1`.
    static func fittedSize(aspectRatio: CGFloat, in container: CGSize) -> CGSize {
        guard aspectRatio > 0, container.width > 0, container.height > 0 else { return .zero }
        let containerRatio = container.width / container.height
        return aspectRatio > containerRatio
            ? CGSize(width: container.width, height: container.width / aspectRatio)
            : CGSize(width: container.height * aspectRatio, height: container.height)
    }

    /// How far the image may be pushed from centre before an edge would come
    /// into view — zero on an axis the image does not overflow.
    static func offsetLimit(fitted: CGSize, container: CGSize, zoom: CGFloat) -> CGSize {
        CGSize(
            width: max(0, (fitted.width * zoom - container.width) / 2),
            height: max(0, (fitted.height * zoom - container.height) / 2)
        )
    }

    /// Clamp an offset to the bounds — used when a gesture ends.
    static func clamped(_ offset: CGSize, limit: CGSize) -> CGSize {
        CGSize(
            width: min(max(offset.width, -limit.width), limit.width),
            height: min(max(offset.height, -limit.height), limit.height)
        )
    }

    /// Damp the part of an offset that is out of bounds — used while a gesture
    /// is live, so the image resists rather than stops dead.
    static func rubberBanded(_ offset: CGSize, limit: CGSize) -> CGSize {
        CGSize(
            width: rubberBanded(offset.width, limit: limit.width),
            height: rubberBanded(offset.height, limit: limit.height)
        )
    }

    private static func rubberBanded(_ value: CGFloat, limit: CGFloat) -> CGFloat {
        let magnitude = abs(value)
        guard magnitude > limit else { return value }
        let damped = limit + (magnitude - limit) * rubberBandFactor
        return value < 0 ? -damped : damped
    }

    /// Clamp a zoom to the allowed range.
    static func clampedZoom(_ zoom: CGFloat) -> CGFloat {
        min(max(zoom, minZoom), maxZoom)
    }

    /// The offset that keeps the content point under `anchor` pinned there while
    /// the zoom changes from `oldZoom` to `newZoom`.
    ///
    /// Derived from `screen = centre + offset + zoom · contentPoint`: solving for
    /// the offset that leaves `contentPoint` unmoved gives
    /// `offset′ = (anchor − centre)(1 − r) + offset · r`, with `r = newZoom /
    /// oldZoom`. That is what makes a pinch grow the photo *around the fingers*
    /// and a double-tap zoom into the spot that was tapped.
    static func anchoredOffset(
        current: CGSize,
        from oldZoom: CGFloat,
        to newZoom: CGFloat,
        anchor: CGPoint,
        container: CGSize
    ) -> CGSize {
        guard oldZoom > 0 else { return current }
        let ratio = newZoom / oldZoom
        let anchorX = anchor.x - container.width / 2
        let anchorY = anchor.y - container.height / 2
        return CGSize(
            width: anchorX * (1 - ratio) + current.width * ratio,
            height: anchorY * (1 - ratio) + current.height * ratio
        )
    }
}
