import CapsuleFoundation
import Foundation

// MARK: - ContentType

/// The asset's media type — a **closed enum per `protocol_version`**, with
/// exactly one canonical value per format (*Metadata — Closed Enum Value Sets*).
///
/// Never an alias: `image/jpg` is not a value, only `image/jpeg`. An alias
/// would produce two content addresses for one format and break dedup.
public enum ContentType: ClosedWireEnum {
    case jpeg
    case png
    case webp
    case gif
    case tiff
    case heic
    case avif
    case jxl
    case dng
    case mp4
    case quicktime
    case matroska
    case webm
    /// A format introduced by a newer `protocol_version`. Readable (the asset
    /// still lists, with a "newer version" indicator); never writable.
    case unknown(String)

    public static let knownCases: [ContentType] = [
        .jpeg, .png, .webp, .gif, .tiff, .heic, .avif, .jxl, .dng,
        .mp4, .quicktime, .matroska, .webm,
    ]

    public init(rawValue: String) {
        self = Self.knownCases.first { $0.rawValue == rawValue } ?? .unknown(rawValue)
    }

    public var rawValue: String {
        switch self {
        case .jpeg: "image/jpeg"
        case .png: "image/png"
        case .webp: "image/webp"
        case .gif: "image/gif"
        case .tiff: "image/tiff"
        case .heic: "image/heic"
        case .avif: "image/avif"
        case .jxl: "image/jxl"
        case .dng: "image/x-adobe-dng"
        case .mp4: "video/mp4"
        case .quicktime: "video/quicktime"
        case .matroska: "video/x-matroska"
        case .webm: "video/webm"
        case let .unknown(raw): raw
        }
    }

    /// The `media_kind` a smart-album predicate queries — image or video,
    /// derived from the content type, never stored separately.
    public var mediaKind: MediaKind {
        switch self {
        case .jpeg, .png, .webp, .gif, .tiff, .heic, .avif, .jxl, .dng: .image
        case .mp4, .quicktime, .matroska, .webm: .video
        case let .unknown(raw): raw.hasPrefix("video/") ? .video : .image
        }
    }

    /// The UI's presentation classification. A Live Photo is *not* derivable
    /// here — it is a stack of an image and a video asset, so
    /// ``StackType/livePhoto`` decides it, not the content type.
    public var presentationMediaType: MediaType {
        mediaKind == .video ? .video : .photo
    }
}

/// Image or video — the derived `media_kind` predicate field.
public enum MediaKind: ClosedWireEnum {
    case image
    case video
    case unknown(String)

    public static let knownCases: [MediaKind] = [.image, .video]

    public init(rawValue: String) {
        self = Self.knownCases.first { $0.rawValue == rawValue } ?? .unknown(rawValue)
    }

    public var rawValue: String {
        switch self {
        case .image: "image"
        case .video: "video"
        case let .unknown(raw): raw
        }
    }
}
