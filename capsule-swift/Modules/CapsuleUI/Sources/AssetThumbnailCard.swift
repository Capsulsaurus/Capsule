import AssetKit
import CapsuleFoundation
import ImagePipeline
import SwiftUI

// MARK: - AssetThumbnailCard

/// One photo as a card: a rounded square with a thin light border.
///
/// The shape a photo takes when it is floating over something else rather than
/// tiling a grid — a fanned stack over a map, a preview beside a row. The
/// border is what makes that work: a photograph dropped straight onto a map
/// bleeds into it, and a card whose edge is only a shadow disappears against a
/// pale background.
///
/// **Always square, whatever the photo is.** A fan of cards at three different
/// aspect ratios reads as debris; a fan of squares reads as a stack. The image
/// fills and is cropped rather than letterboxed, so the subject survives.
///
/// No glass. `CapsuleGlass` is for the navigation and control layer, never over
/// photo content — so depth here comes from the border and a shadow.
public struct AssetThumbnailCard: View {
    private let asset: Asset
    private let side: CGFloat
    private let thumbnails: any ThumbnailProvider

    @State private var image: PlatformImage?

    /// - Parameters:
    ///   - side: the card's edge length in points. Explicit rather than
    ///     inferred from the container: these are drawn over a map, where
    ///     nothing proposes a sensible size.
    public init(asset: Asset, side: CGFloat, thumbnails: any ThumbnailProvider) {
        self.asset = asset
        self.side = side
        self.thumbnails = thumbnails
    }

    public var body: some View {
        // The rectangle is sized first and the artwork hung off it, rather than
        // both being siblings in a `ZStack`: a `scaledToFill` image reports an
        // ideal size taken from the image, and in a stack that size sizes the
        // stack, which is how a square tile silently becomes a 3:4 one.
        Rectangle()
            .fill(.fill.secondary)
            .frame(width: side, height: side)
            .overlay {
                if let image {
                    Image(platformImage: image)
                        .resizable()
                        .scaledToFill()
                }
            }
            .clipped()
            .clipShape(RoundedRectangle(cornerRadius: radius, style: .continuous))
            .overlay {
                RoundedRectangle(cornerRadius: radius, style: .continuous)
                    .strokeBorder(
                        CapsuleTheme.Colors.cardBorder,
                        lineWidth: CapsuleTheme.Stroke.hairline
                    )
            }
            .shadow(color: .black.opacity(0.25), radius: 6, y: 2)
            .task(id: asset.id) { await load() }
    }

    /// Proportional to the card, so a small fan card and a large preview read as
    /// the same shape rather than the small one looking like a pill.
    private var radius: CGFloat { max(CapsuleTheme.Radius.small, side * 0.14) }

    private func load() async {
        guard image == nil else { return }
        let scale = 2.0
        image = await thumbnails.thumbnail(
            for: asset,
            pixelSize: CGSize(width: side * scale, height: side * scale)
        )
    }

    /// An empty card, for a stack whose photos have not arrived yet.
    ///
    /// The same shape, border and shadow as a real one, so a fan does not
    /// change size or position when its contents land.
    public static func placeholder(side: CGFloat) -> some View {
        let radius = max(CapsuleTheme.Radius.small, side * 0.14)
        return RoundedRectangle(cornerRadius: radius, style: .continuous)
            .fill(.fill.secondary)
            .frame(width: side, height: side)
            .overlay {
                RoundedRectangle(cornerRadius: radius, style: .continuous)
                    .strokeBorder(
                        CapsuleTheme.Colors.cardBorder,
                        lineWidth: CapsuleTheme.Stroke.hairline
                    )
            }
            .shadow(color: .black.opacity(0.25), radius: 6, y: 2)
    }
}
