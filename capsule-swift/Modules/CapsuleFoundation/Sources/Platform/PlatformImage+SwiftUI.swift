import CoreGraphics
import SwiftUI

// The SwiftUI half of the image shim.
//
// SwiftUI spells the bitmap initializer differently per platform —
// `Image(uiImage:)` against `Image(nsImage:)` — and neither name exists on the
// other, so a shared view body cannot call either directly. These two members
// are what let every view in the app render a decoded image with one spelling
// and no `#if`.
//
// They are public and live here, next to the `PlatformImage` typealias, because
// three separate modules reached for them independently; a per-module copy
// would either duplicate the branch or make call sites ambiguous.

public extension Image {
    /// Build a SwiftUI image from the platform's bitmap image type.
    ///
    /// Goes through the native initializer rather than `Image(decorative:)` over
    /// a `CGImage`, because the native one keeps whatever orientation the
    /// decoder attached — a bare `CGImage` drops it and photos come out rotated.
    init(platformImage: PlatformImage) {
        #if canImport(UIKit)
            self.init(uiImage: platformImage)
        #else
            self.init(nsImage: platformImage)
        #endif
    }
}

public extension PlatformImage {
    /// The image's size in points, as SwiftUI lays it out.
    ///
    /// Distinct from ``pixelSize``, which reports the backing bitmap's
    /// dimensions and therefore ignores both the scale factor and any EXIF
    /// orientation. Layout maths — an aspect-fit rectangle, a zoom bound —
    /// wants this one; a decode request wants ``pixelSize``.
    var displaySize: CGSize { size }
}
