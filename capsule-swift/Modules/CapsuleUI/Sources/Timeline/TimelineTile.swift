import CapsuleDomain
import CapsuleFoundation
import CoreGraphics
import SwiftUI

// MARK: - TimelineTile

/// One cell of the virtualized timeline.
///
/// Its whole job is to be **correct while the row it shows does not exist yet**.
/// At 250 000 assets the store holds a few thousand rows, so on any fast scroll
/// most tiles on screen are addressing an index that has not arrived. A grid
/// that renders nothing for those flashes white holes across the viewport; one
/// that renders a spinner flashes worse.
///
/// So the tile reads through ``AssetWindowStore/element(at:)`` — whose `nil` is
/// a normal answer, not an error — and paints the best rung it has. The read
/// also *is* the subscription: `element(at:)` touches the store's `revision`, so
/// when the page lands, this body re-runs and the tile fills in. No reload, no
/// reconfigure, no index-path bookkeeping.
struct TimelineTile: View {
    let globalIndex: Int
    let store: AssetWindowStore<LibraryAsset>
    let context: TimelineGridContext
    let images: any TimelineImageSource

    var body: some View {
        // Reading here is what subscribes this tile to the page arriving.
        let asset = store.element(at: globalIndex)
        ZStack {
            content(for: asset)
            selectionOverlay(for: asset)
        }
        .clipped()
        .contentShape(Rectangle())
    }

    @ViewBuilder
    private func content(for asset: LibraryAsset?) -> some View {
        if let asset {
            // Identity by asset, not by index: a recycled cell handed a
            // different asset must start from that asset's own dominant colour,
            // never from the previous photo's pixels.
            TimelineTileImage(asset: asset, context: context, images: images)
                .id(asset.id)
            AssetCellOverlay(asset, showsCullFlag: context.showsCullFlags && !context.isSelecting)
        } else {
            // Not resident. The layout already knows this tile's exact frame, so
            // the grid's geometry is right even here — only its pixels are
            // pending.
            Rectangle().fill(.quaternary)
        }
    }

    /// The multi-select tick, in the **top-leading** corner.
    ///
    /// ``AssetCellOverlay`` owns the other three corners and assigns them by
    /// meaning, so the tick takes the one reserved for transient, mode-scoped
    /// marks of user intent — the same corner a culling pass uses, and the two
    /// modes are mutually exclusive by construction.
    @ViewBuilder
    private func selectionOverlay(for asset: LibraryAsset?) -> some View {
        if context.isSelecting, let asset {
            let selected = context.isSelected(asset.id)
            ZStack(alignment: .topLeading) {
                if selected { Color.black.opacity(0.12) }
                Image(systemName: selected ? "checkmark.circle.fill" : "circle")
                    .font(.system(size: 18))
                    .symbolRenderingMode(.hierarchical)
                    .foregroundStyle(selected ? Color.accentColor : CapsuleTheme.Colors.onMedia)
                    .shadow(radius: 1, y: 0.5)
                    .padding(CapsuleTheme.Spacing.xSmall)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            .allowsHitTesting(false)
        }
    }
}

// MARK: - TimelineTileImage

/// The progressive representation ladder for one asset, as a view.
///
/// The rungs are stacked, cheapest at the back, and each one that arrives covers
/// the one below it:
///
/// 1. ``Lqip/dominantColor`` — no decode, no `await`, no cache. Painted in the
///    first layout pass, which is why a tile is never blank.
/// 2. The LQIP itself — embedded in the metadata blob, so it costs no request.
/// 3. The grid thumbnail — the real picture, when the pipeline has decoded it.
///
/// This is not a loading spinner dressed up. It is the product's own degrade
/// ladder (``RepresentationTier``) used as a loading strategy, so a tile that is
/// *permanently* on rung 1 — an asset whose thumbnail this device does not hold
/// and cannot fetch — looks exactly like a tile that is momentarily on rung 1,
/// and neither looks broken.
private struct TimelineTileImage: View {
    let asset: LibraryAsset
    let context: TimelineGridContext
    let images: any TimelineImageSource

    @State private var placeholder: PlatformImage?
    @State private var thumbnail: PlatformImage?

    var body: some View {
        ZStack {
            dominantColour
            layer(placeholder)
            layer(thumbnail)
        }
        // Keyed on the decode size alone: the asset is this view's identity, so
        // a resize is the only thing that can invalidate what has been loaded.
        .task(id: context.decodeSize) { await load() }
    }

    /// Rung zero. Always drawn, always underneath.
    private var dominantColour: some View {
        Rectangle().fill(fill)
    }

    private var fill: Color {
        guard let colour = asset.lqip?.dominantColor else { return Color.secondary.opacity(0.2) }
        return Color(
            red: Double(colour.red) / 255,
            green: Double(colour.green) / 255,
            blue: Double(colour.blue) / 255
        )
    }

    @ViewBuilder
    private func layer(_ image: PlatformImage?) -> some View {
        if let image {
            Image(platformImage: image)
                .resizable()
                .scaledToFill()
        }
    }

    private func load() async {
        await loadPlaceholder()
        await loadThumbnail()
    }

    private func loadPlaceholder() async {
        // `holds(.lqip)` is the honest gate: without the bytes locally there is
        // nothing to decode, and asking anyway would spend a hop per tile on
        // every fling to be told so.
        guard placeholder == nil, asset.lqip != nil, asset.representations.holds(.lqip) else { return }
        let decoded = await images.lqipImage(for: asset)
        guard !Task.isCancelled else { return }
        placeholder = decoded
    }

    private func loadThumbnail() async {
        let size = context.decodeSize
        // `.zero` means the grid has not been measured yet; decoding at that size
        // would be wasted work and would poison any size-keyed cache.
        guard size.width > 0, size.height > 0 else { return }
        let decoded = await images.thumbnail(for: asset, pixelSize: size)
        guard !Task.isCancelled else { return }
        thumbnail = decoded
    }
}
