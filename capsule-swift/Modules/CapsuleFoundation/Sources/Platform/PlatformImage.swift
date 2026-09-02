#if canImport(UIKit)
    import UIKit
#elseif canImport(AppKit)
    import AppKit
#endif

import CoreGraphics
import Foundation

#if canImport(UIKit)

    /// The platform's bitmap image type — `PlatformImage` on iOS/iPadOS, `NSImage` on
    /// macOS.
    ///
    /// This alias is the whole reason `CapsuleUI`, `ImagePipeline`, and every
    /// feature module can be written once and compiled for all three
    /// destinations. Modules above this one name `PlatformImage`; only this file
    /// and its siblings in `Platform/` may `import UIKit` or `import AppKit`,
    /// which a SwiftLint rule enforces.
    public typealias PlatformImage = UIImage

    /// The platform's device-colour type.
    public typealias PlatformColor = UIColor

#elseif canImport(AppKit)

    public typealias PlatformImage = NSImage
    public typealias PlatformColor = NSColor

#endif

public extension PlatformImage {
    /// Wrap a `CGImage` at a given scale, hiding the two platforms' very
    /// different initializers.
    ///
    /// `PlatformImage` takes a scale directly; `NSImage` takes a point size, so the
    /// pixel dimensions are divided by the scale to land on the same physical
    /// size. Callers get one spelling.
    static func fromCGImage(_ cgImage: CGImage, scale: CGFloat = 1) -> PlatformImage {
        #if canImport(UIKit)
            return UIImage(cgImage: cgImage, scale: scale, orientation: .up)
        #else
            let size = CGSize(
                width: CGFloat(cgImage.width) / scale,
                height: CGFloat(cgImage.height) / scale
            )
            return NSImage(cgImage: cgImage, size: size)
        #endif
    }

    /// The image's underlying `CGImage`, if it has one.
    ///
    /// Always present on iOS for a bitmap-backed image; on macOS an `NSImage`
    /// may be vector- or multi-representation-backed, so this renders the best
    /// representation for the image's own size.
    var platformCGImage: CGImage? {
        #if canImport(UIKit)
            return cgImage
        #else
            var rect = CGRect(origin: .zero, size: size)
            return cgImage(forProposedRect: &rect, context: nil, hints: nil)
        #endif
    }

    /// The image's size in pixels, independent of scale.
    var pixelSize: CGSize {
        guard let cgImage = platformCGImage else { return .zero }
        return CGSize(width: cgImage.width, height: cgImage.height)
    }
}
