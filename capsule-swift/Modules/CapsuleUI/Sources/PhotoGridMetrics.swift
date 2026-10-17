import CoreGraphics
import Foundation

/// The pure geometry behind the photo grid: how a style becomes a layout, and
/// how wide a thumbnail has to be decoded for it.
///
/// Kept free of SwiftUI and of any platform type so the numbers that decide
/// image quality and memory cost are covered by tests instead of by squinting
/// at a simulator.
enum PhotoGridMetrics {
    /// The gap between uniform tiles, in points.
    static let tileSpacing: CGFloat = 1.5
    /// A representative card's height as a fraction of the grid width.
    static let cardHeightRatio: CGFloat = 0.62
    /// A representative card's horizontal / vertical section inset, in points.
    static let cardHorizontalInset: CGFloat = 12
    static let cardVerticalInset: CGFloat = 6
    /// Cards decode slightly taller than they display, so the aspect-fill crop
    /// has pixels to work with.
    static let cardDecodeRatio: CGFloat = 0.7

    /// Decode sizes are rounded up to a multiple of this many pixels.
    ///
    /// Without it, every point of a live window resize on macOS — and every
    /// column-count animation frame on iOS — would be a distinct decode size and
    /// would re-request every visible thumbnail. Quantising costs a few percent
    /// of oversampling and buys a stable cache key.
    static let decodeQuantum: CGFloat = 64

    /// The device-pixel size a cell should decode to.
    ///
    /// - Parameters:
    ///   - containerWidth: the grid's width in points.
    ///   - style: the grid style, which decides how the width is divided.
    ///   - displayScale: points-to-pixels for the screen the grid is on.
    /// - Returns: `.zero` before the grid has been measured, which callers treat
    ///   as "do not decode yet" rather than as a request for a zero-sized image.
    static func decodeSize(
        containerWidth: CGFloat,
        style: PhotoGridStyle,
        displayScale: CGFloat
    ) -> CGSize {
        guard containerWidth > 0 else { return .zero }
        let scale = max(displayScale, 1)
        switch style {
        case let .uniform(columns):
            let side = quantized(containerWidth / CGFloat(max(1, columns)) * scale)
            return CGSize(width: side, height: side)
        case .cards:
            let width = quantized(containerWidth * scale)
            return CGSize(width: width, height: (width * cardDecodeRatio).rounded(.up))
        }
    }

    /// Round `value` up to the next ``decodeQuantum``, never below one quantum.
    static func quantized(_ value: CGFloat) -> CGFloat {
        guard value > 0 else { return 0 }
        return max(decodeQuantum, (value / decodeQuantum).rounded(.up) * decodeQuantum)
    }
}

extension PhotoGridStyle {
    /// The platform-neutral layout this style asks the collection for.
    func platformLayout(pinnedHeaders: Bool) -> PlatformCollectionLayout {
        switch self {
        case let .uniform(columns):
            .uniformGrid(
                columns: max(1, columns),
                itemSpacing: PhotoGridMetrics.tileSpacing,
                pinnedHeaders: pinnedHeaders
            )
        case .cards:
            // Cards are one per section and carry their own title, so a pinned
            // header would just repeat it.
            .fullWidthRows(
                heightRatio: PhotoGridMetrics.cardHeightRatio,
                horizontalInset: PhotoGridMetrics.cardHorizontalInset,
                verticalInset: PhotoGridMetrics.cardVerticalInset
            )
        }
    }
}
