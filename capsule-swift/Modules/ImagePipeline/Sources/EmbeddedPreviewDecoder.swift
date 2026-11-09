import CapsuleDomain
import CapsuleFoundation
import CoreGraphics
import CoreImage
import ImageIO

// MARK: - EmbeddedPreviewDecoder

/// Draws a RAW original's embedded preview (*Thumbnails — Embedded Previews*).
///
/// The preview's ``EmbeddedPreview/orientation`` is the RAW's, and it applies
/// **in place of** any tag the JPEG itself carries: a camera commonly writes
/// the preview untagged and records the orientation on the RAW. So the JPEG is
/// decoded *without* ImageIO's transform, which would read the JPEG's own tag,
/// and the RAW's orientation is applied afterwards.
public enum EmbeddedPreviewDecoder {
    /// The preview decoded with its longest edge at most `maxPixelEdge` and
    /// stood upright, or `nil` when the JPEG does not decode — which leaves the
    /// rung to the next source rather than failing it.
    public static func image(_ preview: EmbeddedPreview, maxPixelEdge: Int) -> CGImage? {
        let signposter = CapsuleSignpost.imagePipeline
        let interval = signposter.beginInterval("embedded-preview")
        defer { signposter.endInterval("embedded-preview", interval) }

        let sourceOptions = [kCGImageSourceShouldCache: false] as CFDictionary
        guard let source = CGImageSourceCreateWithData(preview.jpeg as CFData, sourceOptions),
              CGImageSourceGetCount(source) > 0
        else {
            CapsuleLog.imagePipeline.debug("embedded preview is not a decodable image")
            return nil
        }
        let options = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: false,
            kCGImageSourceShouldCacheImmediately: true,
            kCGImageSourceThumbnailMaxPixelSize: max(1, maxPixelEdge),
        ] as CFDictionary
        guard let stored = CGImageSourceCreateThumbnailAtIndex(source, 0, options) else {
            CapsuleLog.imagePipeline.debug("embedded preview did not decode")
            return nil
        }
        return upright(stored, exifOrientation: preview.orientation)
    }

    /// `image` with the EXIF orientation `exifOrientation` (1...8) applied.
    /// Orientation 1, and any value outside the EXIF range, leaves it as it is.
    static func upright(_ image: CGImage, exifOrientation: UInt16) -> CGImage? {
        guard exifOrientation != 1,
              let orientation = CGImagePropertyOrientation(rawValue: UInt32(exifOrientation))
        else { return image }
        let oriented = CIImage(cgImage: image).oriented(orientation)
        return context.createCGImage(oriented, from: oriented.extent)
    }

    /// One context for every orientation: creating a `CIContext` is the
    /// expensive part, and rendering through a shared one is thread-safe.
    private nonisolated(unsafe) static let context = CIContext(options: [.cacheIntermediates: false])
}
