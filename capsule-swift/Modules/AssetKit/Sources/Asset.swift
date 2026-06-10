import CapsuleFoundation
import Foundation

/// A photo or video, unified across both backing sources.
///
/// `Asset` is the lightweight value type the timeline grid and viewer render.
/// It carries only what those screens need to lay out and badge a tile; heavy
/// or rarely-needed detail (full EXIF, location) is fetched separately when a
/// viewer opens. Its ``id`` records which provider owns it, so any follow-up
/// request — load image, toggle favourite — routes back to the right source.
public struct Asset: Sendable, Identifiable, Equatable, Hashable, Codable {
    /// The source-tagged identifier; also selects the owning provider.
    public var id: AssetID
    /// How the UI should present and play this asset.
    public var mediaType: MediaType
    /// When the asset was captured (device-local wall-clock).
    public var captureDate: Date
    /// Native pixel width; `0` when unknown.
    public var pixelWidth: Int
    /// Native pixel height; `0` when unknown.
    public var pixelHeight: Int
    /// Playback duration in seconds — the video length, or a Live Photo's
    /// motion-clip length; `0` for a still photo.
    public var duration: TimeInterval
    /// Whether the user has favourited the asset.
    public var isFavorite: Bool

    public init(
        id: AssetID,
        mediaType: MediaType,
        captureDate: Date,
        pixelWidth: Int = 0,
        pixelHeight: Int = 0,
        duration: TimeInterval = 0,
        isFavorite: Bool = false
    ) {
        self.id = id
        self.mediaType = mediaType
        self.captureDate = captureDate
        self.pixelWidth = pixelWidth
        self.pixelHeight = pixelHeight
        self.duration = duration
        self.isFavorite = isFavorite
    }

    /// Width ÷ height, falling back to `1` (square) when dimensions are unknown
    /// — used by the grid's aspect-aware layout.
    public var aspectRatio: Double {
        guard pixelWidth > 0, pixelHeight > 0 else { return 1 }
        return Double(pixelWidth) / Double(pixelHeight)
    }

    /// Whether the asset is backed by the system Photos library.
    public var isFromPhotoKit: Bool { id.isPhotoKit }

    /// Whether the asset is backed by the Capsule-managed store.
    public var isManaged: Bool { id.isManaged }
}
