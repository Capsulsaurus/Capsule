import AssetKit
import CapsuleUI
import ImagePipeline
import SwiftUI
import UIKit

/// One album tile in the Collections cover grid: a square cover thumbnail with
/// the title and count beneath.
///
/// The cover is the album's declared `coverAssetID` (managed albums set one)
/// or, failing that, the album's newest asset — loaded lazily so off-screen
/// tiles in a `LazyVGrid` cost nothing until they appear.
struct AlbumCoverCard: View {
    let album: AlbumSummary
    let albumProvider: any AlbumProvider
    let assetProvider: any AssetProvider
    let thumbnails: any ThumbnailProvider
    @State private var cover: UIImage?

    var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xSmall) {
            coverImage
            Text(album.title)
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(.primary)
                .lineLimit(1)
            Text("^[\(album.count) Photo](inflect: true)")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .task(id: album.id) { await loadCover() }
    }

    private var coverImage: some View {
        ZStack {
            Color(.secondarySystemBackground)
            if let cover {
                Image(uiImage: cover)
                    .resizable()
                    .scaledToFill()
            } else {
                Image(systemName: album.isUserAlbum ? "rectangle.stack" : "sparkles.rectangle.stack")
                    .font(.largeTitle)
                    .foregroundStyle(.secondary)
            }
        }
        .aspectRatio(1, contentMode: .fit)
        .clipShape(RoundedRectangle(cornerRadius: CapsuleTheme.Radius.card))
    }

    private func loadCover() async {
        guard cover == nil else { return }
        let pixels = CGSize(width: 500, height: 500)
        if let coverID = album.coverAssetID,
           let asset = try? await assetProvider.asset(for: coverID) {
            cover = await thumbnails.thumbnail(for: asset, pixelSize: pixels)
            return
        }
        let assets = (try? await albumProvider.assets(in: album.id)) ?? []
        guard let asset = assets.first else { return }
        cover = await thumbnails.thumbnail(for: asset, pixelSize: pixels)
    }
}

/// A single tappable row in the Collections Utilities / More groups.
struct CollectionRow: View {
    let systemImage: String
    let title: String

    var body: some View {
        HStack(spacing: CapsuleTheme.Spacing.medium) {
            Image(systemName: systemImage)
                .font(.body)
                .foregroundStyle(.tint)
                .frame(width: 28)
            Text(title).foregroundStyle(.primary)
            Spacer()
            Image(systemName: "chevron.right")
                .font(.caption.weight(.semibold))
                .foregroundStyle(.tertiary)
        }
        .padding(.horizontal, CapsuleTheme.Spacing.medium)
        .padding(.vertical, CapsuleTheme.Spacing.medium)
        .contentShape(Rectangle())
    }
}
