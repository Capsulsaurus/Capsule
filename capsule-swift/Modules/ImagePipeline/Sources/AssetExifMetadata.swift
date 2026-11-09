import AssetKit

/// How a file encodes high dynamic range, when it does.
///
/// A closed set on purpose. These are the three states the info panel can say
/// something true about; a fourth format nobody recognises should read as
/// "no HDR row" rather than as a name the reader cannot act on.
public enum HDRFormat: String, Sendable, Equatable, CaseIterable {
    case hdr10
    case dolbyVision
    case hlg
}

/// Camera, exposure, and file metadata shown in the viewer's info panel.
///
/// Read from an asset's embedded EXIF, its container, and its PhotoKit
/// resource — or, in the mock lane, derived from the asset's own seed. Every
/// field is optional because every one of them is genuinely absent for some
/// real file: a scan has no lens, a still has no frame rate, an export has no
/// original filename.
public struct AssetExifMetadata: Sendable, Equatable {
    // Camera
    public var cameraMake: String?
    public var cameraModel: String?
    public var lensModel: String?
    /// How the camera describes the lens in use — "Main Camera", "Ultra Wide".
    /// Distinct from ``lensModel``, which is the hardware part.
    public var lensName: String?
    public var isoSpeed: Int?
    public var aperture: Double?
    public var shutterSpeed: Double?
    public var focalLength: Double?

    // File
    /// The name the file had when it was captured or imported.
    public var originalFilename: String?
    /// Size on disk, in bytes.
    public var byteCount: Int64?
    /// The codec the bytes are in — "HEVC", "H.264", "HEIF", "JPEG".
    ///
    /// A display string rather than the sidecar's `content_type`: the container
    /// MIME says `video/quicktime` where the reader wants to know it is HEVC,
    /// and those are genuinely different facts about the same file.
    public var codec: String?
    /// Frames per second, for time-based media.
    public var frameRate: Double?
    /// Which HDR encoding the file uses, when it uses one.
    public var hdrFormat: HDRFormat?

    // Location
    public var latitude: Double?
    public var longitude: Double?

    public init() {}

    /// Whether any *camera* field could be read.
    ///
    /// Scoped to the camera card rather than the whole struct: a file that knows
    /// its size and codec but nothing about the lens should still show its file
    /// line, and asking "is the whole thing empty" would hide it.
    public var hasCameraDetail: Bool {
        cameraMake != nil || cameraModel != nil || lensModel != nil || lensName != nil
            || isoSpeed != nil || aperture != nil || shutterSpeed != nil || focalLength != nil
    }

    /// Whether any file-level fact could be read.
    public var hasFileDetail: Bool {
        originalFilename != nil || byteCount != nil || codec != nil
            || frameRate != nil || hdrFormat != nil
    }

    /// Whether nothing at all could be read.
    public var isEmpty: Bool {
        !hasCameraDetail && !hasFileDetail && latitude == nil && longitude == nil
    }
}

// MARK: - AssetMetadataSource

/// Where the info panel's metadata comes from when the asset is not a PhotoKit
/// one.
///
/// The same seam ``ThumbnailProvider`` is for pixels, and it exists for the same
/// reason: `ViewerMediaLoader` reaches PhotoKit through an identifier only a
/// system asset carries, so a managed or mocked photo produced an entirely empty
/// panel — the camera and location sections silently vanished, which reads as
/// "this photo has no metadata" rather than as "this lane cannot see it".
public protocol AssetMetadataSource: Sendable {
    /// Metadata for an asset this source owns, or `nil` if it does not own it.
    func metadata(for asset: Asset) async -> AssetExifMetadata?
}
