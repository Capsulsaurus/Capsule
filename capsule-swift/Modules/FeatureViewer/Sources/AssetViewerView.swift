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
    private let captionStore: (any CaptionStore)?
    @Environment(\.dismiss) private var dismiss
    @State private var isAddToAlbumPresented = false
    /// Ties the bottom bar's glass groups together so they morph as one.
    @Namespace private var barNamespace
    /// Whether the info panel should open expanded.
    ///
    /// What separates Info from Adjust. They present the same sheet, because
    /// the caption and the location are *in* that sheet and a second surface
    /// holding two fields would be a worse answer than a detent. Info opens it
    /// at half height to read; Adjust opens it fully, where the editable fields
    /// are reachable without a drag. Two buttons that presented the identical
    /// thing would be one button drawn twice.
    @State private var infoPanelStartsExpanded = false

    public init(
        assets: [Asset],
        startIndex: Int,
        provider: any AssetProvider,
        mediaLoader: ViewerMediaLoader,
        albumProvider: any AlbumProvider,
        captionStore: (any CaptionStore)? = nil
    ) {
        _model = State(wrappedValue: AssetViewerModel(
            assets: assets,
            startIndex: startIndex,
            provider: provider,
            albumProvider: albumProvider
        ))
        self.mediaLoader = mediaLoader
        self.captionStore = captionStore
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
                AssetInfoPanel(
                    asset: asset,
                    mediaLoader: mediaLoader,
                    captionStore: captionStore,
                    startsExpanded: infoPanelStartsExpanded
                )
            }
        }
        .confirmationDialog(
            "app.add_to_album.title",
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
                ? LocalizedStringKey("app.add_to_album.empty_albums")
                : LocalizedStringKey("app.add_to_album.choose"))
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
        .accessibilityLabel("app.common.done")
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
                        ? LocalizedStringKey("app.viewer.pause_slideshow")
                        : LocalizedStringKey("app.viewer.play_slideshow"),
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
                    Label("app.add_to_album.title", systemImage: "rectangle.stack.badge.plus")
                }
            }
        } label: {
            Image(systemName: "ellipsis")
                .font(.headline)
                .foregroundStyle(.white)
                .padding(10)
                .capsuleGlass(in: Circle(), interactive: true)
        }
        .accessibilityLabel("app.viewer.more")
        .accessibilityIdentifier("viewer.more")
    }

    // MARK: Bottom chrome

    /// What you can do to *this* photo, in three glass groups.
    ///
    /// Share, then the trio that acts on the photo in place — keep it, learn
    /// about it, change it — then, held well apart, throw it away. That is the
    /// arrangement iOS 26's own Photos uses, and the grouping is the argument:
    /// an equal-width run of five identical glyphs makes the destructive one
    /// exactly as easy to hit by accident as the others, and a viewer is a place
    /// where fingers move fast.
    ///
    /// Share leaves the pill for the same reason it is first: it sends the photo
    /// somewhere else, which is a different kind of act from editing it in
    /// place, and a `ShareLink` in an equal-width run stretches oddly against
    /// its neighbours.
    ///
    /// `CapsuleGlassContainer` groups all three so they blend and morph as one —
    /// glass cannot sample glass — and the glass is `.clear` because these are
    /// small controls floating over media, which is the case that variant was
    /// written for and had never been used in.
    private var bottomBar: some View {
        CapsuleGlassContainer(spacing: CapsuleTheme.Spacing.medium) {
            HStack(spacing: CapsuleTheme.Spacing.medium) {
                shareButton
                    .fixedSize()
                    .padding(CapsuleTheme.Spacing.medium)
                    .capsuleGlass(.clear, in: Circle())
                    .capsuleGlassID(BarGroup.share, in: barNamespace)

                HStack(spacing: 0) {
                    barButton(favoriteSymbol, tint: favoriteTint) {
                        Task { await model.toggleFavorite() }
                    }
                    .accessibilityLabel("app.viewer.favorite")
                    barButton("info.circle") { presentInfoPanel(expanded: false) }
                        .accessibilityLabel("app.viewer.info")
                        .accessibilityIdentifier("viewer.info")
                    barButton("slider.horizontal.3") { presentInfoPanel(expanded: true) }
                        .accessibilityLabel("app.viewer.info.adjust")
                        .accessibilityIdentifier("viewer.adjust")
                }
                .frame(maxWidth: .infinity)
                .padding(.vertical, CapsuleTheme.Spacing.medium)
                .padding(.horizontal, CapsuleTheme.Spacing.small)
                .capsuleGlass(.clear, in: Capsule())
                .capsuleGlassID(BarGroup.actions, in: barNamespace)

                barButton("trash") {
                    Task {
                        if await model.deleteCurrentAsset() { dismiss() }
                    }
                }
                .accessibilityLabel("app.viewer.delete")
                .accessibilityIdentifier("viewer.delete")
                .fixedSize()
                .padding(CapsuleTheme.Spacing.medium)
                .capsuleGlass(.clear, in: Circle())
                .capsuleGlassID(BarGroup.delete, in: barNamespace)
            }
        }
        .padding(.horizontal, CapsuleTheme.Spacing.large)
        .padding(.bottom, CapsuleTheme.Spacing.small)
    }

    /// The bar's three glass groups, named so they can morph rather than
    /// cross-fade when the bar changes shape.
    private enum BarGroup: Hashable {
        case share
        case actions
        case delete
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
                Image(systemName: "square.and.arrow.up")
                    .font(CapsuleTheme.Typography.controlGlyph)
                    .foregroundStyle(CapsuleTheme.Colors.onMedia)
            }
        }
    }

    private func presentInfoPanel(expanded: Bool) {
        infoPanelStartsExpanded = expanded
        model.isInfoPanelPresented = true
    }

    private func barButton(
        _ symbol: String,
        tint: Color = CapsuleTheme.Colors.onMedia,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            barLabel(symbol, tint: tint)
        }
    }

    private func barLabel(_ symbol: String, tint: Color = CapsuleTheme.Colors.onMedia) -> some View {
        Image(systemName: symbol)
            .font(CapsuleTheme.Typography.controlGlyph)
            .foregroundStyle(tint)
            .frame(maxWidth: .infinity)
    }

    private var favoriteSymbol: String {
        isCurrentFavorite ? "heart.fill" : "heart"
    }

    private var favoriteTint: Color {
        isCurrentFavorite ? CapsuleTheme.Colors.favorite : CapsuleTheme.Colors.onMedia
    }

    private var isCurrentFavorite: Bool {
        model.currentAsset?.isFavorite ?? false
    }
}
