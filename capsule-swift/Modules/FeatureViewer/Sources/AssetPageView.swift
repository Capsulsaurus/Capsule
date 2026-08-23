import AssetKit
import AVKit
import CapsuleFoundation
import CapsuleUI
import ImagePipeline
import Photos
import SwiftUI

/// One page of the viewer, dispatched by media type.
struct AssetPageView: View {
    let asset: Asset
    let mediaLoader: ViewerMediaLoader

    var body: some View {
        switch asset.mediaType {
        case .photo:
            PhotoPage(asset: asset, mediaLoader: mediaLoader)
        case .livePhoto:
            LivePhotoPage(asset: asset, mediaLoader: mediaLoader)
        case .video:
            VideoPage(asset: asset, mediaLoader: mediaLoader)
        }
    }
}

// MARK: - Photo

private struct PhotoPage: View {
    let asset: Asset
    let mediaLoader: ViewerMediaLoader
    @Environment(\.displayScale) private var displayScale
    @State private var image: PlatformImage?

    var body: some View {
        GeometryReader { geometry in
            ZStack {
                if let image {
                    ZoomableImageView(image: image)
                        .accessibilityIdentifier("viewer.image")
                } else {
                    ProgressView().tint(.white)
                        .accessibilityIdentifier("viewer.loading")
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .task(id: asset.id) {
                // Decode with 2× headroom over the screen so pinch-zoom stays sharp.
                let pixels = CGSize(
                    width: geometry.size.width * displayScale * 2,
                    height: geometry.size.height * displayScale * 2
                )
                image = await mediaLoader.fullImage(for: asset, targetSize: pixels)
            }
        }
    }
}

// MARK: - Live Photo

private struct LivePhotoPage: View {
    let asset: Asset
    let mediaLoader: ViewerMediaLoader
    @Environment(\.displayScale) private var displayScale
    @State private var livePhoto: PHLivePhoto?
    @State private var playbackTicket = 0

    var body: some View {
        GeometryReader { geometry in
            ZStack {
                if let livePhoto {
                    LivePhotoView(livePhoto: livePhoto, playbackTicket: playbackTicket)
                } else {
                    ProgressView().tint(.white)
                }
            }
            // Over the media, where the convention puts a playback control —
            // and where it was missing entirely: the motion played once on
            // appear and there was no way, anywhere in the app, to see it
            // again.
            .overlay(alignment: .topLeading) {
                if livePhoto != nil {
                    liveBadge
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .task(id: asset.id) {
                let pixels = CGSize(
                    width: geometry.size.width * displayScale,
                    height: geometry.size.height * displayScale
                )
                livePhoto = await mediaLoader.livePhoto(for: asset, targetSize: pixels)
            }
        }
    }

    /// Apple's own badge, made live: tapping it replays the motion.
    private var liveBadge: some View {
        Button { playbackTicket += 1 } label: {
            Label("app.viewer.live.replay", systemImage: "livephoto")
                .font(.caption.weight(.semibold))
                .labelStyle(.titleAndIcon)
                .foregroundStyle(CapsuleTheme.Colors.onMedia)
                .padding(.horizontal, CapsuleTheme.Spacing.small)
                .padding(.vertical, CapsuleTheme.Spacing.xSmall)
                .background(.black.opacity(0.35), in: Capsule())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("app.viewer.live.replay.accessibility")
        .accessibilityIdentifier("viewer.live.replay")
        .padding(CapsuleTheme.Spacing.large)
    }
}

// MARK: - Video

private struct VideoPage: View {
    /// Roughly the height of the viewer's floating action bar plus its bottom
    /// padding. Approximate on purpose: the exact figure depends on Dynamic
    /// Type, and the cost of a few points too many is whitespace while the cost
    /// of too few is a buried scrubber.
    static let chromeClearance: CGFloat = 88

    let asset: Asset
    let mediaLoader: ViewerMediaLoader
    @State private var player: AVPlayer?

    var body: some View {
        ZStack {
            if let player {
                VideoPlayer(player: player)
                    // AVKit lays its transport controls against the bottom of
                    // its own bounds, and the viewer's action bar floats over
                    // that same strip — so the scrubber and the play button
                    // were rendering *underneath* the chrome. Reserving the
                    // chrome's height pushes AVKit's controls clear of it
                    // rather than fighting it with a bespoke transport.
                    .safeAreaPadding(.bottom, Self.chromeClearance)
            } else {
                ProgressView().tint(.white)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .task(id: asset.id) {
            guard let item = await mediaLoader.playerItem(for: asset) else { return }
            player = AVPlayer(playerItem: item)
        }
        .onDisappear { player?.pause() }
    }
}
