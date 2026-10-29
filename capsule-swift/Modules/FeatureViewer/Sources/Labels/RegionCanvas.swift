import AssetKit
import CapsuleDomain
import CapsuleFoundation
import CapsuleUI
import ImagePipeline
import SwiftUI

// MARK: - RegionCanvas

/// The asset's preview with its regions drawn over it, and the draft being
/// drawn.
///
/// A region is selected by tapping it. The draft moves with a drag and
/// resizes with a pinch, and every edit goes through ``RegionDraft``, so it
/// never leaves the image. VoiceOver reaches the same edits as named actions
/// that move it by ``RegionDraft/step`` and an adjustable action that grows
/// and shrinks it, the TUI's arrow-key steps.
struct RegionCanvas: View {
    let asset: Asset
    let mediaLoader: ViewerMediaLoader?
    let model: LabelEditorModel
    @State private var image: PlatformImage?
    /// The draft as it was when the current drag or pinch began.
    @State private var gestureBase: RegionDraft?

    var body: some View {
        GeometryReader { geometry in
            let bounds = CGRect(origin: .zero, size: geometry.size)
            let imageFrame = RegionGeometry.aspectFit(imageSize, in: bounds)
            ZStack(alignment: .topLeading) {
                Color.black
                preview
                    .frame(width: imageFrame.width, height: imageFrame.height)
                    .position(x: imageFrame.midX, y: imageFrame.midY)
                ForEach(model.regions) { region in
                    regionOverlay(region, in: imageFrame)
                }
                if let draft = model.draft {
                    draftOverlay(draft, in: imageFrame)
                }
            }
            .clipped()
        }
        .task(id: asset.id) {
            image = await mediaLoader?.fullImage(for: asset, targetSize: CGSize(width: 2048, height: 2048))
        }
    }

    /// The image's size as laid out: the decoded image's, else the asset's
    /// recorded pixels, else a square.
    private var imageSize: CGSize {
        if let image { return image.displaySize }
        if asset.pixelWidth > 0, asset.pixelHeight > 0 {
            return CGSize(width: asset.pixelWidth, height: asset.pixelHeight)
        }
        return CGSize(width: 1, height: 1)
    }

    @ViewBuilder
    private var preview: some View {
        if let image {
            Image(platformImage: image)
                .resizable()
                .accessibilityHidden(true)
        } else {
            Text("app.labels.preview.unavailable")
                .font(.footnote)
                .foregroundStyle(CapsuleTheme.Colors.onMedia)
        }
    }

    // MARK: Regions

    private func regionOverlay(_ region: AssetRegion, in imageFrame: CGRect) -> some View {
        let frame = RegionGeometry.frame(of: region.shape, in: imageFrame)
        let isSelected = region.id == model.selectedRegionID
        return outline(for: region.shape.isCircle)
            .stroke(isSelected ? Color.yellow : Color.white, lineWidth: isSelected ? 3 : 2)
            .contentShape(outline(for: region.shape.isCircle))
            .frame(width: frame.width, height: frame.height)
            .position(x: frame.midX, y: frame.midY)
            .shadow(radius: 2)
            .onTapGesture { model.selectRegion(region.id) }
            .accessibilityElement()
            .accessibilityLabel(region.displayLabels.map { Text(verbatim: $0) } ?? Text("app.labels.region.unlabelled"))
            .accessibilityAddTraits(isSelected ? [.isButton, .isSelected] : .isButton)
            .accessibilityAction { model.selectRegion(region.id) }
    }

    // MARK: Draft

    private func draftOverlay(_ draft: RegionDraft, in imageFrame: CGRect) -> some View {
        let frame = RegionGeometry.frame(of: draft, in: imageFrame)
        return outline(for: draft.kind == .circle)
            .stroke(Color.accentColor, style: StrokeStyle(lineWidth: 3, dash: [6, 4]))
            .background(outline(for: draft.kind == .circle).fill(Color.accentColor.opacity(0.15)))
            .contentShape(outline(for: draft.kind == .circle))
            .frame(width: frame.width, height: frame.height)
            .position(x: frame.midX, y: frame.midY)
            .gesture(drag(in: imageFrame))
            .simultaneousGesture(pinch)
            .accessibilityElement()
            .accessibilityLabel("app.labels.draft.label")
            .accessibilityHint("app.labels.draft.hint")
            .accessibilityAdjustableAction { direction in
                switch direction {
                case .increment: nudge { $0.resized(dw: RegionDraft.step, dh: RegionDraft.step) }
                case .decrement: nudge { $0.resized(dw: -RegionDraft.step, dh: -RegionDraft.step) }
                @unknown default: break
                }
            }
            .accessibilityAction(named: Text("app.labels.draft.move_left")) {
                nudge { $0.moved(dx: -RegionDraft.step, dy: 0) }
            }
            .accessibilityAction(named: Text("app.labels.draft.move_right")) {
                nudge { $0.moved(dx: RegionDraft.step, dy: 0) }
            }
            .accessibilityAction(named: Text("app.labels.draft.move_up")) {
                nudge { $0.moved(dx: 0, dy: -RegionDraft.step) }
            }
            .accessibilityAction(named: Text("app.labels.draft.move_down")) {
                nudge { $0.moved(dx: 0, dy: RegionDraft.step) }
            }
    }

    private func nudge(_ edit: (RegionDraft) -> RegionDraft) {
        if let draft = model.draft { model.draft = edit(draft) }
    }

    private func drag(in imageFrame: CGRect) -> some Gesture {
        DragGesture()
            .onChanged { value in
                let base = gestureBase ?? model.draft
                gestureBase = base
                let step = RegionGeometry.fractions(of: value.translation, in: imageFrame)
                model.draft = base?.moved(dx: step.dx, dy: step.dy)
            }
            .onEnded { _ in gestureBase = nil }
    }

    private var pinch: some Gesture {
        MagnifyGesture()
            .onChanged { value in
                let base = gestureBase ?? model.draft
                gestureBase = base
                model.draft = base?.scaled(by: value.magnification)
            }
            .onEnded { _ in gestureBase = nil }
    }

    private func outline(for isCircle: Bool) -> AnyShape {
        isCircle ? AnyShape(Circle()) : AnyShape(Rectangle())
    }
}

// MARK: - RegionShape

extension RegionShape {
    var isCircle: Bool {
        if case .circle = self { return true }
        return false
    }
}
