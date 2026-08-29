import AssetKit
import CapsuleUI
import ImagePipeline
import SwiftUI

// The two grouped cards the info panel is built from.
//
// Cards rather than a `List` of label/value rows. The rows were legible but they
// were a *table*: every fact given equal weight, read top to bottom, with the
// camera's name no more prominent than its ISO. A photo's metadata is not a
// table — it is three or four short statements, and grouping them the way the
// camera itself would say them is what makes the panel scannable rather than
// merely complete.

// MARK: - Camera card

/// Everything about how the photograph was taken, as one grouped surface.
struct AssetInfoCameraCard: View {
    let asset: Asset
    let metadata: AssetExifMetadata

    var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.small) {
            if let name = cameraName {
                HStack(spacing: CapsuleTheme.Spacing.small) {
                    Text(name)
                        .font(CapsuleTheme.Typography.cardTitle)
                        .accessibilityLabel(Text("app.viewer.info.model"))
                        .accessibilityValue(name)
                    Spacer(minLength: CapsuleTheme.Spacing.small)
                    if let codec = metadata.codec {
                        Text(codec)
                            .font(CapsuleTheme.Typography.badge)
                            .accessibilityLabel(Text("app.viewer.info.type"))
                            .accessibilityValue(codec)
                    }
                    Image(systemName: asset.mediaType == .photo ? "camera" : "video")
                        .font(CapsuleTheme.Typography.detail)
                        .accessibilityHidden(true)
                }
            }
            if let lens = lensLine {
                Text(lens)
                    .font(CapsuleTheme.Typography.detail)
                    .foregroundStyle(.secondary)
                    .accessibilityLabel(Text("app.viewer.info.lens"))
                    .accessibilityValue(lens)
            }
            if let file = fileLine {
                Text(file)
                    .font(CapsuleTheme.Typography.detail)
                    .foregroundStyle(.secondary)
                    .accessibilityLabel(Text("app.viewer.info.dimensions"))
                    .accessibilityValue(file)
            }
            if let hdr = metadata.hdrFormat {
                Label(
                    AssetInfoFormatting.hdrName(hdr),
                    systemImage: "sparkles.tv"
                )
                .font(CapsuleTheme.Typography.detail)
                .foregroundStyle(.secondary)
            }
            if let footer = playbackFooter {
                Divider()
                HStack {
                    Spacer()
                    Text(footer.left).accessibilityLabel(Text("app.viewer.info.duration"))
                    Spacer()
                    Divider().frame(height: CapsuleTheme.Spacing.large)
                    Spacer()
                    Text(footer.right)
                    Spacer()
                }
                .font(CapsuleTheme.Typography.numeric)
                .foregroundStyle(.secondary)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(CapsuleTheme.Spacing.medium)
        .background(.quaternary, in: RoundedRectangle(cornerRadius: CapsuleTheme.Radius.card))
    }

    /// Whether the card has anything at all to say.
    ///
    /// Checked by the caller, so an asset with no camera detail gets no empty
    /// rounded rectangle where a card would be.
    var hasContent: Bool {
        cameraName != nil || lensLine != nil || fileLine != nil
            || metadata.hdrFormat != nil || playbackFooter != nil
    }

    private var cameraName: String? {
        AssetInfoFormatting.cameraName(make: metadata.cameraMake, model: metadata.cameraModel)
    }

    private var lensLine: String? {
        AssetInfoFormatting.lensLine(
            name: metadata.lensName ?? metadata.lensModel,
            focalLength: metadata.focalLength,
            aperture: metadata.aperture
        )
    }

    private var fileLine: String? {
        AssetInfoFormatting.fileLine(
            resolutionClass: AssetInfoFormatting.resolutionClass(
                width: asset.pixelWidth, height: asset.pixelHeight
            ),
            dimensions: AssetInfoFormatting.dimensions(
                width: asset.pixelWidth, height: asset.pixelHeight
            ),
            fileSize: metadata.byteCount.flatMap(AssetInfoFormatting.fileSize)
        )
    }

    /// Frame rate and running time, the pair a video card ends on.
    ///
    /// Both or neither: a lone figure in a two-column footer reads as a missing
    /// value rather than as an absent one.
    private var playbackFooter: (left: String, right: String)? {
        guard let rate = metadata.frameRate.flatMap(AssetInfoFormatting.frameRate),
              let duration = AssetInfoFormatting.duration(asset.duration)
        else { return nil }
        return (rate, duration)
    }
}

// MARK: - Location card

/// Where the photograph was taken: a map, and the name of the place when the
/// reader has asked for names to be resolved.
struct AssetInfoLocationCard: View {
    let coordinate: AssetCoordinate
    /// The resolved locality, when there is one to show.
    let placeName: String?
    let onAdjust: (() -> Void)?

    var body: some View {
        VStack(spacing: 0) {
            AssetInfoMap(coordinate: coordinate)
                .frame(height: 160)
                .accessibilityLabel(Text("app.common.location"))
            if placeName != nil || onAdjust != nil {
                HStack {
                    if let placeName {
                        Text(placeName)
                            .font(CapsuleTheme.Typography.detail)
                    }
                    Spacer(minLength: CapsuleTheme.Spacing.small)
                    if let onAdjust {
                        Button("app.viewer.info.adjust", action: onAdjust)
                            .font(CapsuleTheme.Typography.detail)
                            .accessibilityLabel(Text("app.viewer.info.adjust_location.accessibility"))
                    }
                }
                .padding(CapsuleTheme.Spacing.medium)
            }
        }
        .background(.quaternary, in: RoundedRectangle(cornerRadius: CapsuleTheme.Radius.card))
        .clipShape(RoundedRectangle(cornerRadius: CapsuleTheme.Radius.card))
    }
}
