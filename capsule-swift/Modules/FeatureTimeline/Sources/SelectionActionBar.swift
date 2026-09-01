import AssetKit
import CapsuleUI
import FeatureViewer
import ImagePipeline
import SwiftUI

/// The floating bar of things you can do to a set of selected photos.
///
/// A component with explicit inputs rather than a `TimelineRootView` extension,
/// because none of what it does depends on the grid: give it some assets and
/// five closures and it renders the same bar on any screen with a selection —
/// album detail and search results want exactly this next.
///
/// It also settles a Swift constraint honestly. `private` is *file*-scoped, so an
/// extension in another file cannot reach a view's `@State`; splitting this out
/// as an extension would have meant widening a dozen properties to internal and
/// calling the result encapsulation. Passing what it needs is both smaller and
/// the reason the bar is reusable at all.
struct SelectionActionBar: View {
    /// The assets the actions apply to. Empty disables the whole bar.
    let assets: [Asset]
    /// How a shared asset gets its bytes, when the user picks a destination.
    let mediaLoader: ViewerMediaLoader
    let onFavorite: () -> Void
    let onAddToAlbum: () -> Void
    let onHide: () -> Void
    let onDelete: () -> Void

    var body: some View {
        HStack(spacing: 0) {
            shareAction
            action("heart", perform: onFavorite)
            action("rectangle.stack.badge.plus", perform: onAddToAlbum)
            action("eye.slash", perform: onHide)
            action("trash", role: .destructive, perform: onDelete)
        }
        .padding(.vertical, CapsuleTheme.Spacing.medium)
        .padding(.horizontal, CapsuleTheme.Spacing.small)
        .capsuleGlass(in: Capsule())
        .padding(.horizontal, CapsuleTheme.Spacing.large)
        .padding(.bottom, CapsuleTheme.Spacing.small)
        .disabled(assets.isEmpty)
    }

    /// Share every selected asset.
    ///
    /// A `ShareLink` rather than a button that pre-loads images into state:
    /// ``ShareableAsset`` decodes each original only once the user has chosen a
    /// destination, so selecting two hundred photos costs nothing until then —
    /// and `ShareLink` is the one share affordance both platforms have.
    private var shareAction: some View {
        ShareLink(
            items: assets.map { ShareableAsset(asset: $0, mediaLoader: mediaLoader) },
            preview: { SharePreview($0.previewTitle) },
            label: { label("square.and.arrow.up") }
        )
    }

    private func action(
        _ symbol: String,
        role: ButtonRole? = nil,
        perform: @escaping () -> Void
    ) -> some View {
        Button(role: role, action: perform) { label(symbol) }
    }

    private func label(_ symbol: String) -> some View {
        Image(systemName: symbol)
            .font(CapsuleTheme.Typography.controlGlyph)
            .frame(maxWidth: .infinity)
    }
}
