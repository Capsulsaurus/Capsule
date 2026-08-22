import AssetKit
import CapsuleUI
import ImagePipeline
import SwiftUI

/// The full-screen, horizontally-paged asset viewer.
///
/// Pages the supplied assets through ``AssetPager`` — a swipe-driven pager on
/// iOS, arrow keys and chevrons on macOS; each page is a zoomable photo, a Live
/// Photo, or a video. A bottom bar offers share, info, add-to-album, favourite,
/// and delete, all routed through ``AssetViewerModel``.
///
/// Presented via ``SwiftUI/View/capsuleFullScreenCover(item:content:)``, so it
/// covers the screen on iOS and arrives as a large sheet on macOS.
public struct AssetViewerView: View {
    @State private var model: AssetViewerModel
    private let mediaLoader: ViewerMediaLoader
    @Environment(\.dismiss) private var dismiss
    @State private var isAddToAlbumPresented = false

    public init(
        assets: [Asset],
        startIndex: Int,
        provider: any AssetProvider,
        mediaLoader: ViewerMediaLoader,
        albumProvider: any AlbumProvider
    ) {
        _model = State(wrappedValue: AssetViewerModel(
            assets: assets,
            startIndex: startIndex,
            provider: provider,
            albumProvider: albumProvider
        ))
        self.mediaLoader = mediaLoader
    }

    public var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()
            pager
        }
        .overlay(alignment: .topLeading) { closeButton }
        .overlay(alignment: .bottom) { bottomBar }
        .capsuleStatusBarHidden()
        .onDisappear { model.stopSlideshow() }
        .sheet(isPresented: $model.isInfoPanelPresented) {
            if let asset = model.currentAsset {
                AssetInfoPanel(asset: asset, mediaLoader: mediaLoader)
            }
        }
        .confirmationDialog(
            "ios.add_to_album.title",
            isPresented: $isAddToAlbumPresented,
            titleVisibility: .visible
        ) {
            ForEach(model.userAlbums) { album in
                Button(album.title) {
                    Task { await model.addCurrentAsset(to: album.id) }
                }
            }
        } message: {
            Text(model.userAlbums.isEmpty
                ? LocalizedStringKey("ios.add_to_album.empty_albums")
                : LocalizedStringKey("ios.add_to_album.choose"))
        }
    }

    @ViewBuilder
    private var pager: some View {
        if model.assets.isEmpty {
            Color.clear.onAppear { dismiss() }
        } else {
            AssetPager(
                assets: model.assets,
                currentIndex: $model.currentIndex,
                mediaLoader: mediaLoader
            )
        }
    }

    private var closeButton: some View {
        Button {
            dismiss()
        } label: {
            Image(systemName: "xmark")
                .font(.headline)
                .foregroundStyle(.white)
                .padding(10)
                .capsuleGlass(in: Circle(), interactive: true)
        }
        .padding(.leading, 16)
        .padding(.top, 8)
    }

    private var bottomBar: some View {
        HStack(spacing: 0) {
            shareButton
            barButton(model.isPlayingSlideshow ? "pause.fill" : "play.fill") {
                model.toggleSlideshow()
            }
            .accessibilityLabel(model.isPlayingSlideshow
                ? LocalizedStringKey("ios.viewer.pause_slideshow")
                : LocalizedStringKey("ios.viewer.play_slideshow"))
            barButton("info.circle") { model.isInfoPanelPresented = true }
            if model.currentAsset?.isManaged == true {
                barButton("rectangle.stack.badge.plus") {
                    Task {
                        await model.loadUserAlbums()
                        isAddToAlbumPresented = true
                    }
                }
            }
            barButton(favoriteSymbol, tint: favoriteTint) {
                Task { await model.toggleFavorite() }
            }
            barButton("trash") {
                Task {
                    if await model.deleteCurrentAsset() { dismiss() }
                }
            }
        }
        .padding(.vertical, CapsuleTheme.Spacing.medium)
        .padding(.horizontal, CapsuleTheme.Spacing.small)
        .capsuleGlass(in: Capsule())
        .padding(.horizontal, CapsuleTheme.Spacing.large)
        .padding(.bottom, CapsuleTheme.Spacing.small)
    }

    /// The share affordance.
    ///
    /// A `ShareLink` rather than a button that loads the image and then presents
    /// a sheet: the export happens inside ``ShareableAsset`` once the user picks
    /// a destination, so there is no intermediate state to hold and the same
    /// code works on both platforms.
    @ViewBuilder
    private var shareButton: some View {
        if let asset = model.currentAsset {
            let shareable = ShareableAsset(asset: asset, mediaLoader: mediaLoader)
            ShareLink(item: shareable, preview: SharePreview(shareable.previewTitle)) {
                barLabel("square.and.arrow.up")
            }
        }
    }

    private func barButton(
        _ symbol: String,
        tint: Color = .white,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            barLabel(symbol, tint: tint)
        }
    }

    private func barLabel(_ symbol: String, tint: Color = .white) -> some View {
        Image(systemName: symbol)
            .font(.title3)
            .foregroundStyle(tint)
            .frame(maxWidth: .infinity)
    }

    private var favoriteSymbol: String {
        isCurrentFavorite ? "heart.fill" : "heart"
    }

    private var favoriteTint: Color {
        isCurrentFavorite ? .red : .white
    }

    private var isCurrentFavorite: Bool {
        model.currentAsset?.isFavorite ?? false
    }
}
