import AssetKit
import CapsuleFoundation
import CapsuleNavigation
import FeatureViewer
import SwiftUI

/// The full-screen viewer reached by *route* rather than by presentation.
///
/// The four in-app entry points — timeline, album detail, search, places — present
/// ``AssetViewerView`` themselves, as a full-screen cover over the grid the user
/// tapped, with the assets they already hold. This is the other door, and until now
/// it was a placeholder: `capsule://asset/<uuid>` parses correctly
/// (`DeepLink.parsePrivate`), resolves to `.viewer`, and landed on `RouteScaffold`.
///
/// A deep link names an asset and nothing about how the user got to it, so the
/// sequence has to be resolved before the viewer can open. ``ViewerContext/library``
/// exists for exactly that case, and is what the parser produces.
extension RouteDestination {
    /// - Note: only `.timeline` sequences resolve here, because only they can be
    ///   answered from ``AssetProvider/loadTimeline()``. That is not a shortcut: the
    ///   deep-link parser is currently the *sole* producer of `.viewer`, and it
    ///   always emits `.library`, which is `.timeline(.default)`. An album, person,
    ///   place or search sequence would each need its own collection query, and
    ///   nothing pushes one yet — so they keep the scaffold rather than get a
    ///   viewer that cannot page past the asset it opened on.
    @ViewBuilder
    func viewerDestination(_ id: AssetID, context: ViewerContext) -> some View {
        if case .timeline = context {
            ResolvedDestination(
                titleKey: context.owningSection.titleKey,
                systemImage: context.owningSection.systemImage,
                resolve: { [provider = environment.assetProvider] () async -> ViewerSequence? in
                    guard let snapshot = try? await provider.loadTimeline() else { return nil }
                    let assets = (0 ..< snapshot.count).map { snapshot.asset(at: $0) }
                    // The index, not just the asset: the viewer pages through the
                    // sequence, so opening on the right frame is the whole point.
                    guard let index = assets.firstIndex(where: { $0.id == id }) else { return nil }
                    return ViewerSequence(assets: assets, startIndex: index)
                },
                content: { sequence in
                    AssetViewerView(
                        assets: sequence.assets,
                        startIndex: sequence.startIndex,
                        provider: environment.assetProvider,
                        mediaLoader: environment.mediaLoader,
                        albumProvider: environment.albumProvider,
                        captionStore: environment.captionStore,
                        placeNames: environment.placeNames
                    )
                }
            )
        } else {
            unbuilt
        }
    }
}

/// A resolved viewer sequence: the assets to page through, and where to start.
///
/// A pair rather than two resolutions because ``ResolvedDestination`` resolves one
/// value, and because an asset found without its position in the sequence is not
/// enough to open a viewer on.
struct ViewerSequence: Sendable {
    let assets: [Asset]
    let startIndex: Int
}
