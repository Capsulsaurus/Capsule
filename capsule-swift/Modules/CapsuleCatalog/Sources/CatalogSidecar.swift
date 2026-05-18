import Foundation

// MARK: - CatalogSidecar

/// The CBOR sidecar paired with every managed media file — a Swift-native,
/// `Sendable` mirror of the Rust `AssetSidecarRecord`.
///
/// The sidecar is the portable, self-describing record of an asset that lives
/// next to the file on disk; the SQLite catalog is a rebuildable index over
/// these. `ManagedStore` builds a `CatalogSidecar` during import, encodes it
/// with ``SidecarCodec``, and writes the bytes beside the media file.
///
/// Type-like fields carry canonical snake_case strings (`assetType`,
/// `importMode`, `captureTimezoneSource`). ``unknownFieldsCBOR`` is an opaque
/// blob of sidecar fields written by a newer build: it is round-tripped
/// verbatim so forward compatibility holds without Swift parsing CBOR itself.
public struct CatalogSidecar: Sendable, Equatable {
    /// The sidecar schema version.
    public var version: UInt8
    /// The asset's catalog UUID.
    public var uuid: String
    /// Canonical catalog asset type: `"photo"`, `"video"`, or `"sidecar"`.
    public var assetType: String
    /// The asset's filename as it was at import time.
    public var originalFilename: String
    /// When the asset was imported, Unix epoch seconds.
    public var importTimestamp: Int64
    /// When the sidecar was last modified, Unix epoch seconds.
    public var modifiedTimestamp: Int64
    /// Lowercase hex SHA-256 of the asset's file bytes.
    public var hashSHA256: String
    /// The media file's size in bytes.
    public var fileSize: UInt64
    /// Whether the asset is soft-deleted.
    public var isDeleted: Bool
    /// User rating, 0–5.
    public var rating: UInt8
    /// Free-form user tags.
    public var tags: [String]
    /// Canonical import mode: `"copy"` or `"move"`.
    public var importMode: String
    /// The version of the importer that wrote this sidecar.
    public var importerVersion: String
    /// The Rawshift pipeline version recorded at import.
    public var rawshiftVersion: String
    /// Device-local capture wall-clock, Unix epoch seconds.
    public var captureTimestamp: Int64?
    /// Capture instant in UTC, Unix epoch seconds.
    public var captureUTC: Int64?
    /// The IANA timezone identifier resolved for the capture.
    public var captureTimezone: String?
    /// How the capture timezone was resolved (`"offset_exif"`, …).
    public var captureTimezoneSource: String?
    /// The timezone database version used for the lookup.
    public var timezoneDatabaseVersion: String?
    /// Pixel width, when known.
    public var width: UInt32?
    /// Pixel height, when known.
    public var height: UInt32?
    /// Duration in milliseconds for time-based media.
    public var durationMillis: UInt64?
    /// The stack-membership hint, when this asset belongs to a stack.
    public var stackHint: CatalogStackHint?
    /// The user album this asset belongs to, if any.
    public var albumID: String?
    /// When the asset was soft-deleted, Unix epoch seconds.
    public var deletedAt: Int64?
    /// The capture device manufacturer, from EXIF.
    public var cameraMake: String?
    /// The capture device model, from EXIF.
    public var cameraModel: String?
    /// GPS latitude in decimal degrees, from EXIF.
    public var gpsLatitude: Double?
    /// GPS longitude in decimal degrees, from EXIF.
    public var gpsLongitude: Double?
    /// Opaque CBOR of sidecar fields this build does not recognise. Empty when
    /// there are none; never inspected, only round-tripped.
    public var unknownFieldsCBOR: Data

    public init(
        version: UInt8,
        uuid: String,
        assetType: String,
        originalFilename: String,
        importTimestamp: Int64,
        modifiedTimestamp: Int64,
        hashSHA256: String,
        fileSize: UInt64,
        isDeleted: Bool = false,
        rating: UInt8 = 0,
        tags: [String] = [],
        importMode: String = "copy",
        importerVersion: String,
        rawshiftVersion: String,
        captureTimestamp: Int64? = nil,
        captureUTC: Int64? = nil,
        captureTimezone: String? = nil,
        captureTimezoneSource: String? = nil,
        timezoneDatabaseVersion: String? = nil,
        width: UInt32? = nil,
        height: UInt32? = nil,
        durationMillis: UInt64? = nil,
        stackHint: CatalogStackHint? = nil,
        albumID: String? = nil,
        deletedAt: Int64? = nil,
        cameraMake: String? = nil,
        cameraModel: String? = nil,
        gpsLatitude: Double? = nil,
        gpsLongitude: Double? = nil,
        unknownFieldsCBOR: Data = Data()
    ) {
        self.version = version
        self.uuid = uuid
        self.assetType = assetType
        self.originalFilename = originalFilename
        self.importTimestamp = importTimestamp
        self.modifiedTimestamp = modifiedTimestamp
        self.hashSHA256 = hashSHA256
        self.fileSize = fileSize
        self.isDeleted = isDeleted
        self.rating = rating
        self.tags = tags
        self.importMode = importMode
        self.importerVersion = importerVersion
        self.rawshiftVersion = rawshiftVersion
        self.captureTimestamp = captureTimestamp
        self.captureUTC = captureUTC
        self.captureTimezone = captureTimezone
        self.captureTimezoneSource = captureTimezoneSource
        self.timezoneDatabaseVersion = timezoneDatabaseVersion
        self.width = width
        self.height = height
        self.durationMillis = durationMillis
        self.stackHint = stackHint
        self.albumID = albumID
        self.deletedAt = deletedAt
        self.cameraMake = cameraMake
        self.cameraModel = cameraModel
        self.gpsLatitude = gpsLatitude
        self.gpsLongitude = gpsLongitude
        self.unknownFieldsCBOR = unknownFieldsCBOR
    }
}

// MARK: - CatalogStackHint

/// A stack-membership hint stored in a sidecar — enough for `ManagedStore` to
/// rebuild the `asset_stacks` / `stack_members` rows from sidecars alone.
public struct CatalogStackHint: Sendable, Equatable {
    /// The key that groups members of the same stack (e.g. an Apple content
    /// identifier, or a shared filename stem).
    public var detectionKey: String
    /// Canonical detection method: `"content_identifier"`, `"filename_stem"`, ….
    public var detectionMethod: String
    /// Canonical member role: `"primary"`, `"video"`, ….
    public var memberRole: String
    /// Canonical stack type: `"live_photo"`, `"burst"`, ….
    public var stackType: String

    public init(
        detectionKey: String,
        detectionMethod: String,
        memberRole: String,
        stackType: String
    ) {
        self.detectionKey = detectionKey
        self.detectionMethod = detectionMethod
        self.memberRole = memberRole
        self.stackType = stackType
    }
}
