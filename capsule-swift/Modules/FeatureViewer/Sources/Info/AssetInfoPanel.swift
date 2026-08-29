import AssetKit
import CapsuleUI
import ImagePipeline
import SwiftUI

/// The info panel: what the reader wrote, when it was taken, and what the camera
/// recorded — in that order.
///
/// A stack of grouped cards over the photograph rather than a `List` of
/// label/value rows. The rows were legible but they were a *table*: every fact
/// at equal weight, the camera's name no more prominent than its ISO, and the
/// photograph itself hidden behind an opaque sheet. A photo's metadata is three
/// or four short statements, and the photograph is the context for all of them —
/// which is why the sheet keeps it visible.
struct AssetInfoPanel: View {
    let asset: Asset
    let mediaLoader: ViewerMediaLoader
    /// Where the caption is read and written. `nil` in a lane with no caption
    /// store, which simply omits the field rather than showing a dead one.
    let captionStore: (any CaptionStore)?
    /// Whether to open at full height, where the editable fields are reachable
    /// without a drag. Set by the Adjust button; Info opens at half.
    var startsExpanded = false

    @Environment(\.dismiss) private var dismiss
    @State private var metadata = AssetExifMetadata()
    @State private var detent: PresentationDetent = .medium

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.large) {
                    if let captionStore {
                        AssetCaptionField(assetID: asset.id, store: captionStore)
                    }
                    captureHeader
                    cameraCard
                    locationCard
                }
                .padding(CapsuleTheme.Spacing.large)
            }
            .scrollBounceBehavior(.basedOnSize)
            .navigationTitle("app.viewer.info.title")
            .capsuleNavigationBarInline()
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("app.common.done") { dismiss() }
                }
            }
        }
        // Dark regardless of the system appearance, because the sheet belongs
        // to the *viewer* rather than to the app: the viewer is a black,
        // full-bleed surface with white chrome in every appearance, and a light
        // panel sliding up over a photograph on it looks like a different app's
        // sheet landed on top.
        .preferredColorScheme(.dark)
        .capsuleMediaSheet(detents: [.medium, .large], selection: $detent)
        .task(id: asset.id) {
            metadata = await mediaLoader.metadata(for: asset)
        }
        .onAppear { detent = startsExpanded ? .large : .medium }
    }

    /// Weekday, date, time — and the filename beneath, the way a file browser
    /// would name it.
    private var captureHeader: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.xxSmall) {
            HStack(alignment: .firstTextBaseline) {
                Text(
                    asset.captureDate,
                    format: .dateTime.weekday(.wide).month(.abbreviated).day().year()
                        .hour().minute()
                )
                .font(CapsuleTheme.Typography.cardTitle)
                Spacer(minLength: CapsuleTheme.Spacing.small)
            }
            .accessibilityElement(children: .combine)
            .accessibilityLabel(Text("app.viewer.info.date"))

            if let filename = metadata.originalFilename {
                Text(filename)
                    .font(CapsuleTheme.Typography.detail)
                    .foregroundStyle(.secondary)
            }
        }
    }

    @ViewBuilder
    private var cameraCard: some View {
        let card = AssetInfoCameraCard(asset: asset, metadata: metadata)
        if card.hasContent { card }
    }

    @ViewBuilder
    private var locationCard: some View {
        if let latitude = metadata.latitude, let longitude = metadata.longitude {
            AssetInfoLocationCard(
                coordinate: AssetCoordinate(latitude: latitude, longitude: longitude),
                placeName: nil,
                onAdjust: nil
            )
        }
    }
}
