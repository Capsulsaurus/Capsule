import Foundation

// MARK: - CatalogAsset

/// One row of the catalog's `assets` table — an imported media file.
///
/// A Swift-native, `Sendable` mirror of the Rust `AssetRecord`. The raw FFI
/// record type never crosses this module's boundary; callers see only this.
/// Type-like fields (`assetType`, `captureTimezoneSource`) carry their
/// canonical snake_case catalog strings verbatim — the catalog layer is a
/// faithful, lossless mirror and leaves domain interpretation to `AssetKit`.
///
/// Timestamps are Unix epoch **seconds**. `captureTimestamp` is device-local
/// wall-clock; `captureUTC`, when known, is the same instant in UTC.
public struct CatalogAsset: Sendable, Equatable, Identifiable {
    /// The asset's stable catalog UUID (UUIDv7 string).
    public var id: String
    /// Canonical catalog asset type: `"photo"`, `"video"`, or `"sidecar"`.
    public var assetType: String
    /// Device-local capture wall-clock, Unix epoch seconds.
    public var captureTimestamp: Int64
    /// Capture instant in UTC, Unix epoch seconds, when the timezone is known.
    public var captureUTC: Int64?
    /// How the capture timezone was resolved (`"offset_exif"`, …), if at all.
    public var captureTimezoneSource: String?
    /// When the asset was imported into the catalog, Unix epoch seconds.
    public var importTimestamp: Int64
    /// Lowercase hex SHA-256 of the asset's file bytes.
    public var hashSHA256: String
    /// Pixel width, when known.
    public var width: Int64?
    /// Pixel height, when known.
    public var height: Int64?
    /// Duration in milliseconds for time-based media.
    public var durationMillis: Int64?
    /// The stack this asset belongs to, if any.
    public var stackID: String?
    /// Whether this asset is hidden from the timeline as a non-cover stack member.
    public var isStackHidden: Bool
    /// Perceptual chroma hash, when computed.
    public var chromahash: String?
    /// Dominant colour as a hex string, when computed.
    public var dominantColor: String?
    /// The user album this managed asset belongs to, if any.
    public var albumID: String?
    /// User rating, 0–5; 0 means unrated.
    public var rating: Int64
    /// Whether the asset is soft-deleted (in the trash).
    public var isDeleted: Bool
    /// When the asset was soft-deleted, Unix epoch seconds.
    public var deletedAt: Int64?

    public init(
        id: String,
        assetType: String,
        captureTimestamp: Int64,
        importTimestamp: Int64,
        hashSHA256: String,
        captureUTC: Int64? = nil,
        captureTimezoneSource: String? = nil,
        width: Int64? = nil,
        height: Int64? = nil,
        durationMillis: Int64? = nil,
        stackID: String? = nil,
        isStackHidden: Bool = false,
        chromahash: String? = nil,
        dominantColor: String? = nil,
        albumID: String? = nil,
        rating: Int64 = 0,
        isDeleted: Bool = false,
        deletedAt: Int64? = nil
    ) {
        self.id = id
        self.assetType = assetType
        self.captureTimestamp = captureTimestamp
        self.importTimestamp = importTimestamp
        self.hashSHA256 = hashSHA256
        self.captureUTC = captureUTC
        self.captureTimezoneSource = captureTimezoneSource
        self.width = width
        self.height = height
        self.durationMillis = durationMillis
        self.stackID = stackID
        self.isStackHidden = isStackHidden
        self.chromahash = chromahash
        self.dominantColor = dominantColor
        self.albumID = albumID
        self.rating = rating
        self.isDeleted = isDeleted
        self.deletedAt = deletedAt
    }

    /// The canonical timeline axis: the UTC capture instant when known, else
    /// the device-local wall-clock. Mirrors the catalog's
    /// `COALESCE(capture_utc, capture_timestamp)` ordering and filtering.
    public var effectiveCaptureTimestamp: Int64 {
        captureUTC ?? captureTimestamp
    }
}

// MARK: - CatalogAlbum

/// One row of the catalog's `albums` table — a user-defined album.
public struct CatalogAlbum: Sendable, Equatable, Identifiable {
    /// The album's stable catalog UUID.
    public var id: String
    /// The user-facing album name.
    public var name: String
    /// When the album was created, Unix epoch seconds.
    public var createdAt: Int64
    /// When the album was last modified, Unix epoch seconds.
    public var modifiedAt: Int64
    /// The asset used as the album's cover, if chosen.
    public var coverAssetID: String?

    public init(
        id: String,
        name: String,
        createdAt: Int64,
        modifiedAt: Int64,
        coverAssetID: String? = nil
    ) {
        self.id = id
        self.name = name
        self.createdAt = createdAt
        self.modifiedAt = modifiedAt
        self.coverAssetID = coverAssetID
    }
}

// MARK: - CatalogStack

/// One row of the catalog's `asset_stacks` table — a group of related assets
/// (a Live Photo, a burst, a RAW+JPEG pair, …).
public struct CatalogStack: Sendable, Equatable, Identifiable {
    /// The stack's stable catalog UUID.
    public var id: String
    /// Canonical stack type: `"live_photo"`, `"burst"`, `"raw_jpeg"`, ….
    public var stackType: String
    /// The asset treated as the stack's primary representative.
    public var primaryAssetID: String
    /// The asset used as the stack's cover, if different from the primary.
    public var coverAssetID: String?
    /// Whether the stack is collapsed to a single tile in the timeline.
    public var isCollapsed: Bool
    /// Whether the stack was formed automatically rather than by the user.
    public var isAutoGenerated: Bool
    /// When the stack was created, Unix epoch seconds.
    public var createdAt: Int64
    /// When the stack was last modified, Unix epoch seconds.
    public var modifiedAt: Int64

    public init(
        id: String,
        stackType: String,
        primaryAssetID: String,
        coverAssetID: String? = nil,
        isCollapsed: Bool = true,
        isAutoGenerated: Bool = true,
        createdAt: Int64,
        modifiedAt: Int64
    ) {
        self.id = id
        self.stackType = stackType
        self.primaryAssetID = primaryAssetID
        self.coverAssetID = coverAssetID
        self.isCollapsed = isCollapsed
        self.isAutoGenerated = isAutoGenerated
        self.createdAt = createdAt
        self.modifiedAt = modifiedAt
    }
}

// MARK: - CatalogStackMember

/// One row of the catalog's `stack_members` table — one asset's membership in
/// a stack, with its ordering and role.
public struct CatalogStackMember: Sendable, Equatable, Identifiable {
    /// The membership row's stable catalog UUID.
    public var id: String
    /// The stack this membership belongs to.
    public var stackID: String
    /// The member asset.
    public var assetID: String
    /// The member's order within the stack.
    public var sequenceOrder: Int64
    /// Canonical member role: `"primary"`, `"video"`, `"raw"`, ….
    public var memberRole: String
    /// When the membership was created, Unix epoch seconds.
    public var createdAt: Int64

    public init(
        id: String,
        stackID: String,
        assetID: String,
        sequenceOrder: Int64,
        memberRole: String,
        createdAt: Int64
    ) {
        self.id = id
        self.stackID = stackID
        self.assetID = assetID
        self.sequenceOrder = sequenceOrder
        self.memberRole = memberRole
        self.createdAt = createdAt
    }
}

// MARK: - TimelineFilter

/// The optional facets applied to a timeline query. Each `nil` facet is not
/// applied; an all-`nil` filter (`.all`) selects the full timeline.
///
/// `assetType` speaks the *catalog* vocabulary (`"photo"` / `"video"` /
/// `"sidecar"`), not the UI's `MediaType`, because one `MediaType` can map to
/// several catalog types.
public struct TimelineFilter: Sendable, Equatable {
    /// Restrict to a single catalog asset type, e.g. `"photo"`.
    public var assetType: String?
    /// Keep only assets whose effective capture instant
    /// (``CatalogAsset/effectiveCaptureTimestamp``) is at or after this Unix
    /// epoch second — inclusive.
    public var capturedAfter: Int64?
    /// Keep only assets whose effective capture instant is at or before this
    /// Unix epoch second — inclusive.
    public var capturedBefore: Int64?

    public init(
        assetType: String? = nil,
        capturedAfter: Int64? = nil,
        capturedBefore: Int64? = nil
    ) {
        self.assetType = assetType
        self.capturedAfter = capturedAfter
        self.capturedBefore = capturedBefore
    }

    /// The unfiltered timeline.
    public static let all = TimelineFilter()

    /// Whether no facet is applied.
    public var isUnfiltered: Bool {
        assetType == nil && capturedAfter == nil && capturedBefore == nil
    }
}
