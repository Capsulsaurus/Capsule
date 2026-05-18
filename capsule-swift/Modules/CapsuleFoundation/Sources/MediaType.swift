import Foundation

/// The kind of media an asset represents, as the app's UI classifies it.
///
/// This is a *presentation* classification: it drives which viewer (still
/// image, Live Photo, or video player) and which grid badge an asset gets.
/// It is deliberately distinct from the catalog's lower-level `asset_type`
/// column (`photo` / `video` / `sidecar`) — a Live Photo is stored as one
/// `photo` asset stacked with one `video` asset, but is a single `.livePhoto`
/// to the UI. The domain layer (`AssetKit`) derives this from the catalog
/// asset type plus stack membership, or from `PHAsset` media subtypes.
public enum MediaType: String, Sendable, Codable, CaseIterable, Hashable {
    /// A still image.
    case photo

    /// A video.
    case video

    /// An Apple Live Photo — a still image paired with a short motion clip.
    case livePhoto

    /// Whether playback (video scrubbing or Live Photo motion) applies.
    public var isMotion: Bool {
        self != .photo
    }
}
