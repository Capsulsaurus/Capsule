import Foundation
import ImageIO
import UniformTypeIdentifiers

/// Intrinsic metadata extracted from a media file during import.
public struct MediaMetadata: Sendable, Equatable {
    /// Canonical catalog asset type — `"photo"` for the current import scope.
    public var assetType: String
    /// Pixel width, when readable.
    public var pixelWidth: Int?
    /// Pixel height, when readable.
    public var pixelHeight: Int?
    /// Capture wall-clock from EXIF `DateTimeOriginal`, Unix epoch seconds.
    public var captureTimestamp: Int64?
    /// Camera manufacturer, from EXIF.
    public var cameraMake: String?
    /// Camera model, from EXIF.
    public var cameraModel: String?
    /// The file size in bytes.
    public var fileSize: Int64

    public init(
        assetType: String = "photo",
        pixelWidth: Int? = nil,
        pixelHeight: Int? = nil,
        captureTimestamp: Int64? = nil,
        cameraMake: String? = nil,
        cameraModel: String? = nil,
        fileSize: Int64 = 0
    ) {
        self.assetType = assetType
        self.pixelWidth = pixelWidth
        self.pixelHeight = pixelHeight
        self.captureTimestamp = captureTimestamp
        self.cameraMake = cameraMake
        self.cameraModel = cameraModel
        self.fileSize = fileSize
    }
}

/// Extracts ``MediaMetadata`` from media file bytes.
///
/// A protocol so the import pipeline can be tested with a deterministic stub
/// rather than real image files.
public protocol MediaMetadataExtracting: Sendable {
    func extractMetadata(from data: Data, filename: String) -> MediaMetadata
}

/// The production extractor — reads container properties via `ImageIO` without
/// decoding pixels.
public struct ImageIOMetadataExtractor: MediaMetadataExtracting {
    public init() {}

    public func extractMetadata(from data: Data, filename _: String) -> MediaMetadata {
        var metadata = MediaMetadata(assetType: "photo", fileSize: Int64(data.count))
        guard let source = CGImageSourceCreateWithData(data as CFData, nil),
              let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any]
        else {
            return metadata
        }
        metadata.pixelWidth = properties[kCGImagePropertyPixelWidth] as? Int
        metadata.pixelHeight = properties[kCGImagePropertyPixelHeight] as? Int
        if let tiff = properties[kCGImagePropertyTIFFDictionary] as? [CFString: Any] {
            metadata.cameraMake = tiff[kCGImagePropertyTIFFMake] as? String
            metadata.cameraModel = tiff[kCGImagePropertyTIFFModel] as? String
        }
        if let exif = properties[kCGImagePropertyExifDictionary] as? [CFString: Any],
           let dateString = exif[kCGImagePropertyExifDateTimeOriginal] as? String {
            metadata.captureTimestamp = Self.parseExifDate(dateString)
        }
        return metadata
    }

    /// Parse an EXIF `yyyy:MM:dd HH:mm:ss` wall-clock string into a Unix epoch,
    /// treating the (timezone-less) wall-clock as UTC for a stable timestamp.
    static func parseExifDate(_ string: String) -> Int64? {
        let fields = string.split(whereSeparator: { $0 == ":" || $0 == " " })
        guard fields.count == 6,
              let year = Int(fields[0]), let month = Int(fields[1]), let day = Int(fields[2]),
              let hour = Int(fields[3]), let minute = Int(fields[4]), let second = Int(fields[5])
        else {
            return nil
        }
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = .gmt
        var components = DateComponents()
        components.year = year
        components.month = month
        components.day = day
        components.hour = hour
        components.minute = minute
        components.second = second
        guard let date = calendar.date(from: components) else { return nil }
        return Int64(date.timeIntervalSince1970)
    }
}
