import CapsuleDomain
import CoreGraphics
import Foundation

// MARK: - RegionGeometry

/// Where a region sits on screen, and how a gesture on screen moves it.
///
/// Regions are fractions of the image over its display orientation
/// (``RegionShape``); the editor draws the image aspect-fit into whatever
/// space it has. These are the conversions between the two, kept pure so the
/// maths a drag or a pinch depends on is tested without a view.
public enum RegionGeometry {
    /// The rectangle an image of `imageSize` occupies when aspect-fit and
    /// centred in `bounds`; empty when either has no area.
    public static func aspectFit(_ imageSize: CGSize, in bounds: CGRect) -> CGRect {
        guard imageSize.width > 0, imageSize.height > 0, bounds.width > 0, bounds.height > 0 else {
            return CGRect(origin: CGPoint(x: bounds.midX, y: bounds.midY), size: .zero)
        }
        let scale = min(bounds.width / imageSize.width, bounds.height / imageSize.height)
        let size = CGSize(width: imageSize.width * scale, height: imageSize.height * scale)
        return CGRect(
            x: bounds.midX - size.width / 2,
            y: bounds.midY - size.height / 2,
            width: size.width,
            height: size.height
        )
    }

    /// The on-screen frame of a draft drawn over `image`: a circle's bounding
    /// square, whose side is its diameter of the image's shorter side.
    public static func frame(of draft: RegionDraft, in image: CGRect) -> CGRect {
        switch draft {
        case let .circle(centerX, centerY, radius):
            let diameter = 2 * radius * Double(min(image.width, image.height))
            return CGRect(
                x: Double(image.minX) + centerX * Double(image.width) - diameter / 2,
                y: Double(image.minY) + centerY * Double(image.height) - diameter / 2,
                width: diameter,
                height: diameter
            )
        case let .rect(left, top, width, height):
            return CGRect(
                x: Double(image.minX) + left * Double(image.width),
                y: Double(image.minY) + top * Double(image.height),
                width: width * Double(image.width),
                height: height * Double(image.height)
            )
        }
    }

    /// The on-screen frame of a stored shape drawn over `image`.
    public static func frame(of shape: RegionShape, in image: CGRect) -> CGRect {
        frame(of: draft(of: shape), in: image)
    }

    /// A stored shape as a draft, in double precision.
    public static func draft(of shape: RegionShape) -> RegionDraft {
        switch shape {
        case let .circle(centerX, centerY, radius):
            .circle(centerX: Double(centerX), centerY: Double(centerY), radius: Double(radius))
        case let .rect(left, top, width, height):
            .rect(left: Double(left), top: Double(top), width: Double(width), height: Double(height))
        }
    }

    /// A drag of `translation` points over `image`, as fractions of its width
    /// and height; zero over an image with no area.
    public static func fractions(of translation: CGSize, in image: CGRect) -> (dx: Double, dy: Double) {
        guard image.width > 0, image.height > 0 else { return (0, 0) }
        return (Double(translation.width / image.width), Double(translation.height / image.height))
    }
}
