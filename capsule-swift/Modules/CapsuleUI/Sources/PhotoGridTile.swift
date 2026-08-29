import AssetKit
import CapsuleFoundation
import Foundation
import ImagePipeline
import SwiftUI

/// A single photo-grid tile: one image layer plus a small media badge.
///
/// Kept deliberately shallow — a fill, an image, and at most two badges — so a
/// fast fling never triggers offscreen rendering. The thumbnail is loaded by a
/// `task` keyed on the asset *and* the decode size, which is what replaces the
/// hand-rolled `Task` cancellation the UIKit cell used to do in
/// `prepareForReuse`: when a recycled cell is handed a different asset, or the
/// grid is resized, SwiftUI cancels the in-flight decode and starts the right
/// one.
struct PhotoGridTile: View {
    let asset: Asset
    let thumbnails: any ThumbnailProvider
    let context: PhotoGridContext

    @State private var image: PlatformImage?

    var body: some View {
        Rectangle()
            .fill(.quaternary)
            .overlay { thumbnail }
            .overlay { selectionDim }
            .overlay(alignment: .bottomLeading) { liveBadge }
            .overlay(alignment: .bottomTrailing) { trailingBadge }
            .clipped()
            .contentShape(Rectangle())
            .modifier(ZoomSource(id: asset.id, namespace: context.zoomNamespace))
            // The tile is the accessibility element — the badge overlay is
            // hidden, so a VoiceOver sweep of a grid reads one element per
            // photo rather than five. The identifier is what lets a UI sweep
            // open a photo at all: before it, no test could reach the viewer.
            .accessibilityElement(children: .ignore)
            .accessibilityIdentifier("grid.tile")
            .accessibilityLabel(Text(asset.captureDate, format: .dateTime.day().month().year()))
            .accessibilityAddTraits(.isButton)
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
                .transition(.opacity)
        }
    }

    @ViewBuilder
    private var selectionDim: some View {
        if context.isSelected(asset.id) {
            Color.black.opacity(0.12)
        }
    }

    @ViewBuilder
    private var liveBadge: some View {
        if asset.mediaType == .livePhoto {
            Image(systemName: "livephoto")
                .font(.caption2.weight(.semibold))
                .foregroundStyle(CapsuleTheme.Colors.onMedia)
                .frame(width: 16, height: 16)
                .mediaScrim()
                .padding(.leading, CapsuleTheme.Spacing.xSmall)
                .padding(.bottom, CapsuleTheme.Spacing.xSmall)
        }
    }

    /// The bottom-trailing corner is shared: in select mode the checkmark owns
    /// it, otherwise a video's duration does. They are never both meaningful,
    /// and overlapping them (as the UIKit cell did) only ever looked like a bug.
    @ViewBuilder
    private var trailingBadge: some View {
        if context.isSelecting {
            Image(systemName: context.isSelected(asset.id) ? "checkmark.circle.fill" : "circle")
                .font(.body)
                .symbolRenderingMode(.hierarchical)
                .foregroundStyle(context.isSelected(asset.id) ? Color.accentColor : CapsuleTheme.Colors.onMedia)
                .shadow(radius: 1, y: 0.5)
                .frame(width: 22, height: 22)
                .padding(.trailing, CapsuleTheme.Spacing.xSmall)
                .padding(.bottom, CapsuleTheme.Spacing.xSmall)
        } else if asset.mediaType == .video {
            Text(Self.durationText(asset.duration))
                .font(.caption2.weight(.semibold).monospacedDigit())
                .foregroundStyle(CapsuleTheme.Colors.onMedia)
                .mediaScrim()
                .padding(.trailing, CapsuleTheme.Spacing.xSmall)
                .padding(.bottom, CapsuleTheme.Spacing.xSmall)
        }
    }

    private func loadThumbnail() async {
        let size = context.decodeSize
        // `.zero` means the grid has not been measured yet; a decode at that
        // size would be wasted work and would poison any size-keyed cache.
        guard size.width > 0, size.height > 0 else { return }
        let loaded = await thumbnails.thumbnail(for: asset, pixelSize: size)
        guard !Task.isCancelled else { return }
        image = loaded
    }

    /// Formats a media duration as `m:ss`. Digits and a colon only — nothing
    /// here is translatable text.
    private static func durationText(_ duration: TimeInterval) -> String {
        let total = Int(duration.rounded())
        return String(format: "%d:%02d", total / 60, total % 60)
    }
}

/// The identity of one thumbnail request. A change in either half means the
/// pixels on screen are wrong and the decode has to be restarted.
private struct DecodeRequest: Equatable {
    let id: AssetID
    let size: CGSize
}

/// Publishes a tile as the thing a zoom transition grows out of, when the grid
/// has a namespace to publish into.
///
/// A `ViewModifier` rather than an inline `if`, because `matchedTransitionSource`
/// changes the view's type and a branch inside the body would need an `AnyView`
/// — in the one view in the app where an extra allocation per cell is least
/// affordable.
private struct ZoomSource: ViewModifier {
    let id: AssetID
    let namespace: Namespace.ID?

    func body(content: Content) -> some View {
        if let namespace {
            content.capsuleZoomSource(id: id, in: namespace)
        } else {
            content
        }
    }
}
