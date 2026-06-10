import AssetKit
import AVKit
import CapsuleUI
import ImagePipeline
import Photos
import PhotosUI
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
    @State private var image: UIImage?

    var body: some View {
        GeometryReader { geometry in
            ZStack {
                if let image {
                    ZoomableImageView(image: image)
                } else {
                    ProgressView().tint(.white)
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

    var body: some View {
        GeometryReader { geometry in
            ZStack {
                if let livePhoto {
                    LivePhotoView(livePhoto: livePhoto)
                } else {
                    ProgressView().tint(.white)
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
}

/// A `PHLivePhotoView` bridged into SwiftUI; plays the motion hint on appear.
private struct LivePhotoView: UIViewRepresentable {
    let livePhoto: PHLivePhoto

    func makeUIView(context _: Context) -> PHLivePhotoView {
        let view = PHLivePhotoView()
        view.contentMode = .scaleAspectFit
        return view
    }

    func updateUIView(_ view: PHLivePhotoView, context _: Context) {
        guard view.livePhoto !== livePhoto else { return }
        view.livePhoto = livePhoto
        view.startPlayback(with: .hint)
    }
}

// MARK: - Video

private struct VideoPage: View {
    let asset: Asset
    let mediaLoader: ViewerMediaLoader
    @State private var player: AVPlayer?

    var body: some View {
        ZStack {
            if let player {
                VideoPlayer(player: player)
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
