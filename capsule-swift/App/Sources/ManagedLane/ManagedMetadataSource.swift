import AssetKit
import CapsuleFoundation
import Foundation
import ImagePipeline
import ManagedStore

/// The info panel's metadata for an import, read from the stored original.
///
/// An import is a real file with real EXIF, so unlike ``MockMetadataSource``
/// nothing here is derived: the camera, exposure, size and GPS position are the
/// ones the file carries, and a field the file lacks stays empty. The location
/// is the file's own fix, because the managed catalog stores none.
///
/// Answers `nil` for an asset the managed catalog does not index, which is
/// what lets it sit in front of the mock source in a ``CompositeMetadataSource``.
/// It lives in the app target for the reason ``ManagedThumbnailProvider`` does.
struct ManagedMetadataSource: AssetMetadataSource {
    private let reader: ManagedMediaReader

    init(reader: ManagedMediaReader) {
        self.reader = reader
    }

    /// An import whose original is missing or unreadable still belongs to the
    /// managed catalog, so it answers empty metadata rather than `nil`: `nil`
    /// would hand it to the mock source, which fabricates a camera and an
    /// exposure for any identifier it is asked about.
    func metadata(for asset: Asset) async -> AssetExifMetadata? {
        guard case let .managed(uuid) = asset.id else { return nil }
        if let facts = await reader.facts(uuid: uuid) {
            return Self.metadata(from: facts)
        }
        guard await reader.owns(uuid: uuid) else { return nil }
        CapsuleLog.app.debug("no readable original for import \(uuid, privacy: .public); showing file-less facts")
        return AssetExifMetadata()
    }

    /// The projection, separate so it is testable without a file on disk.
    static func metadata(from facts: ManagedMediaFacts) -> AssetExifMetadata {
        var metadata = AssetExifMetadata()
        metadata.cameraMake = facts.cameraMake
        metadata.cameraModel = facts.cameraModel
        metadata.lensModel = facts.lensModel
        metadata.isoSpeed = facts.isoSpeed
        metadata.aperture = facts.aperture
        metadata.shutterSpeed = facts.shutterSpeed
        metadata.focalLength = facts.focalLength
        metadata.byteCount = facts.byteCount
        metadata.codec = codec(forExtension: facts.fileExtension)
        metadata.latitude = facts.latitude
        metadata.longitude = facts.longitude
        return metadata
    }

    /// The codec a stored extension implies, for the formats the importer
    /// accepts. Anything else is no codec row rather than a guess.
    static func codec(forExtension fileExtension: String) -> String? {
        switch fileExtension {
        case "jpg", "jpeg": "JPEG"
        case "heic", "heif": "HEIF"
        case "png": "PNG"
        case "dng": "DNG"
        case "tif", "tiff": "TIFF"
        case "gif": "GIF"
        case "webp": "WebP"
        case "avif": "AVIF"
        default: nil
        }
    }
}
