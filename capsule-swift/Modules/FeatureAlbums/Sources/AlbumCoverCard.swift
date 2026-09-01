import AssetKit
import CapsuleFoundation
import CapsuleUI
import ImagePipeline
import SwiftUI

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
    @State private var cover: PlatformImage?

    var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xSmall) {
            coverImage
            Text(album.title)
                .font(.subheadline.weight(.semibold))
                .foregroundStyle(.primary)
                .lineLimit(1)
            Text(String(
                localized: "app.albums.photo_count",
                defaultValue: "\(album.count) Photo"
            ))
            .font(.caption)
            .foregroundStyle(.secondary)
        }
        .task(id: album.id) { await loadCover() }
    }

    /// The square cover.
    ///
    /// A `Rectangle` sized first and the artwork hung off it as an `overlay`,
    /// rather than both as siblings in a `ZStack`. A `scaledToFill` image
    /// reports an ideal size derived from the *image*, and inside a `ZStack`
    /// that size participates in sizing the stack — so the tile grew to the
    /// thumbnail's proportions, came out at 3:4 instead of square, and the
    /// second row of the grid drew on top of the first. An `overlay` is
    /// measured against its host and cannot do that.
    ///
    /// `.clipped()` as well as the `clipShape`: the shape trims the corners,
    /// but a `scaledToFill` image still paints outside the frame until
    /// something says otherwise.
    private var coverImage: some View {
        // `.fill.secondary` rather than `Color(.secondarySystemBackground)`:
        // that colour is a `UIColor`, and the semantic SwiftUI fill styles
        // resolve to the right grouped-background tone on both platforms.
        Rectangle()
            .fill(.fill.secondary)
            .aspectRatio(1, contentMode: .fit)
            .overlay {
                if let cover {
                    Image(platformImage: cover)
                        .resizable()
                        .scaledToFill()
                } else {
                    Image(
                        systemName: album.isUserAlbum
                            ? "rectangle.stack"
                            : "sparkles.rectangle.stack"
                    )
                    .font(.largeTitle)
                    .foregroundStyle(.secondary)
                }
            }
            .clipped()
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
        let assets = await (try? albumProvider.assets(in: album.id)) ?? []
        guard let asset = assets.first else { return }
        cover = await thumbnails.thumbnail(for: asset, pixelSize: pixels)
    }
}
