import AssetKit
import CapsuleUI
import ImagePipeline
import SwiftUI
import UIKit

/// The full-screen, horizontally-paged asset viewer.
///
/// Pages the supplied assets in a `TabView`; each page is a zoomable photo, a
/// Live Photo, or a video. A bottom bar offers share, info, add-to-album,
/// favourite, and delete, all routed through ``AssetViewerModel``.
public struct AssetViewerView: View {
    @State private var model: AssetViewerModel
    private let mediaLoader: ViewerMediaLoader
    @Environment(\.dismiss) private var dismiss
    @State private var shareImage: UIImage?
    @State private var isSharePresented = false
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
        .statusBarHidden()
        .onDisappear { model.stopSlideshow() }
        .sheet(isPresented: $model.isInfoPanelPresented) {
            if let asset = model.currentAsset {
                AssetInfoPanel(asset: asset, mediaLoader: mediaLoader)
            }
        }
        .sheet(isPresented: $isSharePresented) {
            if let shareImage {
                ActivityView(items: [shareImage])
            }
        }
        .confirmationDialog(
            "Add to Album",
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
                ? "Create an album in the Albums tab first."
                : "Choose a Capsule album.")
        }
    }

    @ViewBuilder
    private var pager: some View {
        if model.assets.isEmpty {
            Color.clear.onAppear { dismiss() }
        } else {
            TabView(selection: $model.currentIndex) {
                ForEach(Array(model.assets.enumerated()), id: \.element.id) { index, asset in
                    AssetPageView(asset: asset, mediaLoader: mediaLoader)
                        .tag(index)
                }
            }
            .tabViewStyle(.page(indexDisplayMode: .never))
            .ignoresSafeArea()
            .animation(.easeInOut(duration: 0.4), value: model.currentIndex)
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
            barButton("square.and.arrow.up", action: share)
            barButton(model.isPlayingSlideshow ? "pause.fill" : "play.fill") {
                model.toggleSlideshow()
            }
            .accessibilityLabel(model.isPlayingSlideshow ? "Pause Slideshow" : "Play Slideshow")
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

    private func barButton(
        _ symbol: String,
        tint: Color = .white,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            Image(systemName: symbol)
                .font(.title3)
                .foregroundStyle(tint)
                .frame(maxWidth: .infinity)
        }
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

    private func share() {
        guard let asset = model.currentAsset else { return }
        Task {
            let pixels = CGSize(width: 3072, height: 3072)
            if let image = await mediaLoader.fullImage(for: asset, targetSize: pixels) {
                shareImage = image
                isSharePresented = true
            }
        }
    }
}

/// A `UIActivityViewController` bridged into SwiftUI for the share sheet.
private struct ActivityView: UIViewControllerRepresentable {
    let items: [UIImage]

    func makeUIViewController(context _: Context) -> UIActivityViewController {
        UIActivityViewController(activityItems: items, applicationActivities: nil)
    }

    func updateUIViewController(_: UIActivityViewController, context _: Context) {}
}
