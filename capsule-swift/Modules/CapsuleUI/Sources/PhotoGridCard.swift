import AssetKit
import CapsuleFoundation
import ImagePipeline
import SwiftUI

/// A large representative card for the Years / Months aggregation levels: one
/// photo standing in for a whole period, with the period title over a
/// legibility gradient.
///
/// Mirrors ``PhotoGridTile``'s discipline — one image layer and a `task`-keyed
/// decode — so the aggregate grid scrolls as smoothly as the tile grid.
struct PhotoGridCard: View {
    let asset: Asset
    let title: String
    let thumbnails: any ThumbnailProvider
    let context: PhotoGridContext

    @State private var image: PlatformImage?

    var body: some View {
        Rectangle()
            .fill(.quaternary)
            .overlay { thumbnail }
            .overlay(alignment: .bottom) { titleOverlay }
            .clipShape(RoundedRectangle(cornerRadius: CapsuleTheme.Radius.card, style: .continuous))
            .contentShape(Rectangle())
            .task(id: DecodeRequest(id: asset.id, size: context.decodeSize)) {
                await loadThumbnail()
            }
    }

    @ViewBuilder
    private var thumbnail: some View {
        if let image {
            Image(platformImage: image)
                .resizable()
                .scaledToFill()
        }
    }

    /// A bottom-to-top scrim keeps the title legible over any photo. This is
    /// deliberately *not* glass: it sits directly on photo content, which is
    /// exactly where the HIG says glass must not go.
    private var titleOverlay: some View {
        Text(title)
            .font(.system(size: 22, weight: .bold))
            .foregroundStyle(CapsuleTheme.Colors.onMedia)
            .shadow(radius: 2, y: 0.5)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, CapsuleTheme.Spacing.large)
            .padding(.bottom, 14)
            .padding(.top, CapsuleTheme.Spacing.xxLarge)
            .background {
                LinearGradient(
                    colors: [.clear, .black.opacity(0.55)],
                    startPoint: .top,
                    endPoint: .bottom
                )
            }
    }

    private func loadThumbnail() async {
        let size = context.decodeSize
        guard size.width > 0, size.height > 0 else { return }
        let loaded = await thumbnails.thumbnail(for: asset, pixelSize: size)
        guard !Task.isCancelled else { return }
        image = loaded
    }
}

/// The identity of one thumbnail request; see ``PhotoGridTile``.
private struct DecodeRequest: Equatable {
    let id: AssetID
    let size: CGSize
}
