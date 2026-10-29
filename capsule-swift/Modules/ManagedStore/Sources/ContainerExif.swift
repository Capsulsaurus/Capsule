import Foundation
import ImageIO

/// The camera, exposure and GPS fields an image container declares, read by
/// ImageIO from its property dictionaries.
///
/// The one parser for these fields: the managed store reads an import's
/// original through it, and the viewer's PhotoKit loader reads a library
/// photo's bytes through it, so the two info panels cannot drift apart on what
/// a file says about itself.
///
/// Every field is optional because every one is genuinely absent for some real
/// file: a screenshot has no lens, a scan has no GPS fix.
public struct ContainerExif: Sendable, Equatable {
    public var cameraMake: String?
    public var cameraModel: String?
    public var lensModel: String?
    public var isoSpeed: Int?
    public var aperture: Double?
    public var shutterSpeed: Double?
    public var focalLength: Double?
    /// Signed decimal degrees: south of the equator is negative.
    public var latitude: Double?
    /// Signed decimal degrees: west of Greenwich is negative.
    public var longitude: Double?

    public init() {}

    /// The fields `data`'s container declares — container properties only,
    /// never a pixel decode. Bytes ImageIO cannot open yield every field empty.
    public static func parse(_ data: Data) -> ContainerExif {
        guard let source = CGImageSourceCreateWithData(data as CFData, nil),
              let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any]
        else { return ContainerExif() }
        return parse(properties: properties)
    }

    /// The fields an already-read property dictionary declares — for a caller
    /// that opened the image source itself and should not open it twice.
    static func parse(properties: [CFString: Any]) -> ContainerExif {
        var exif = ContainerExif()
        if let tiff = properties[kCGImagePropertyTIFFDictionary] as? [CFString: Any] {
            exif.cameraMake = tiff[kCGImagePropertyTIFFMake] as? String
            exif.cameraModel = tiff[kCGImagePropertyTIFFModel] as? String
        }
        if let fields = properties[kCGImagePropertyExifDictionary] as? [CFString: Any] {
            exif.lensModel = fields[kCGImagePropertyExifLensModel] as? String
            exif.isoSpeed = (fields[kCGImagePropertyExifISOSpeedRatings] as? [Int])?.first
            exif.aperture = fields[kCGImagePropertyExifFNumber] as? Double
            exif.shutterSpeed = fields[kCGImagePropertyExifExposureTime] as? Double
            exif.focalLength = fields[kCGImagePropertyExifFocalLength] as? Double
        }
        if let gps = properties[kCGImagePropertyGPSDictionary] as? [CFString: Any] {
            exif.latitude = signedDegrees(
                gps[kCGImagePropertyGPSLatitude] as? Double,
                reference: gps[kCGImagePropertyGPSLatitudeRef] as? String,
                negative: "S"
            )
            exif.longitude = signedDegrees(
                gps[kCGImagePropertyGPSLongitude] as? Double,
                reference: gps[kCGImagePropertyGPSLongitudeRef] as? String,
                negative: "W"
            )
        }
        return exif
    }

    /// EXIF stores GPS as an unsigned magnitude plus a hemisphere letter.
    private static func signedDegrees(_ magnitude: Double?, reference: String?, negative: String) -> Double? {
        guard let magnitude else { return nil }
        return reference?.uppercased() == negative ? -abs(magnitude) : abs(magnitude)
    }
}
