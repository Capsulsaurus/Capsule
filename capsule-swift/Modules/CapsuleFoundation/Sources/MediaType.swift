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

    /// This kind of media, named for display.
    ///
    /// Resolved from the catalog rather than returned as a key, because the
    /// info panel puts it in a value column beside a labelled row — it is text,
    /// not something SwiftUI will look up. The key below is where the text
    /// comes from.
    public var displayName: String {
        String(localized: String.LocalizationValue(displayNameKey))
    }

    /// The catalog key naming this kind of media.
    ///
    /// A key rather than the text. This used to return English directly, and
    /// because it reached the screen through a variable rather than a `Text`
    /// literal, `i18n-guard` never saw it — so every non-English user read
    /// "Live Photo" in English while the gate reported no findings.
    public var displayNameKey: String {
        switch self {
        case .photo: "app.media.photo"
        case .video: "app.media.video"
        case .livePhoto: "app.media.live_photo"
        }
    }
}
