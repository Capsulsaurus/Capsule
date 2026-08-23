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
        .overlay(alignment: .top) { topBar }
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

    // MARK: Top chrome

    /// Close, what you are looking at, and everything that is not about *this*
    /// photo.
    ///
    /// The top bar used to be a lone close button: the viewer never said which
    /// photo was on screen, and a paged viewer without a date is a viewer you
    /// can get lost in.
    private var topBar: some View {
        HStack(alignment: .center) {
            closeButton
            Spacer(minLength: CapsuleTheme.Spacing.small)
            title
            Spacer(minLength: CapsuleTheme.Spacing.small)
            overflowMenu
        }
        .padding(.horizontal, CapsuleTheme.Spacing.large)
        .padding(.top, CapsuleTheme.Spacing.small)
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
        .accessibilityLabel("ios.common.done")
    }

    @ViewBuilder
    private var title: some View {
        if let date = model.currentAsset?.captureDate {
            VStack(spacing: 0) {
                Text(date, format: .dateTime.day().month(.abbreviated).year())
                    .font(.footnote.weight(.semibold))
                Text(date, format: .dateTime.hour().minute())
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
            .foregroundStyle(CapsuleTheme.Colors.onMedia)
            .shadow(radius: 3)
            .accessibilityIdentifier("viewer.title")
        }
    }

    /// Actions on the *sequence* or the library, not on the photo.
    ///
    /// Slideshow lived in the bottom bar behind a `play.fill`, next to five
    /// per-asset actions and colliding with the one symbol every viewer uses
    /// for playing the thing on screen. It plays the whole sequence, so it
    /// belongs with the other things that are not about this photo.
    private var overflowMenu: some View {
        Menu {
            Button {
                model.toggleSlideshow()
            } label: {
                Label(
                    model.isPlayingSlideshow
                        ? LocalizedStringKey("ios.viewer.pause_slideshow")
                        : LocalizedStringKey("ios.viewer.play_slideshow"),
                    systemImage: model.isPlayingSlideshow ? "pause" : "play"
                )
            }
            if model.currentAsset?.isManaged == true {
                Button {
                    Task {
                        await model.loadUserAlbums()
                        isAddToAlbumPresented = true
                    }
                } label: {
                    Label("ios.add_to_album.title", systemImage: "rectangle.stack.badge.plus")
                }
            }
        } label: {
            Image(systemName: "ellipsis")
                .font(.headline)
                .foregroundStyle(.white)
                .padding(10)
                .capsuleGlass(in: Circle(), interactive: true)
        }
        .accessibilityLabel("ios.viewer.more")
        .accessibilityIdentifier("viewer.more")
    }

    // MARK: Bottom chrome

    /// What you can do to *this* photo, in the order the platform trained
    /// people to expect: give it away, keep it, learn about it — and, held
    /// apart, throw it away.
    ///
    /// Four slots rather than six. Slideshow and Add to Album moved to the
    /// overflow menu because neither acts on the photo, and playback moved onto
    /// the media itself; what is left is the set every photo viewer on the
    /// platform puts here.
    ///
    /// Delete sits in its own glass group with a gap before it. An equal-width
    /// run of six identical glyphs makes the destructive one exactly as easy to
    /// hit by accident as the others, and a viewer is a place where fingers
    /// move fast. `CapsuleGlassContainer` groups them because glass cannot
    /// sample glass.
    private var bottomBar: some View {
        CapsuleGlassContainer(spacing: CapsuleTheme.Spacing.medium) {
            HStack(spacing: CapsuleTheme.Spacing.medium) {
                HStack(spacing: 0) {
                    shareButton
                    barButton(favoriteSymbol, tint: favoriteTint) {
                        Task { await model.toggleFavorite() }
                    }
                    .accessibilityLabel("ios.viewer.favorite")
                    barButton("info.circle") { model.isInfoPanelPresented = true }
                        .accessibilityLabel("ios.viewer.info")
                }
                .frame(maxWidth: .infinity)
                .padding(.vertical, CapsuleTheme.Spacing.medium)
                .padding(.horizontal, CapsuleTheme.Spacing.small)
                .capsuleGlass(in: Capsule())

                barButton("trash") {
                    Task {
                        if await model.deleteCurrentAsset() { dismiss() }
                    }
                }
                .accessibilityLabel("ios.viewer.delete")
                .accessibilityIdentifier("viewer.delete")
                .fixedSize()
                .padding(CapsuleTheme.Spacing.medium)
                .capsuleGlass(in: Circle())
            }
        }
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
