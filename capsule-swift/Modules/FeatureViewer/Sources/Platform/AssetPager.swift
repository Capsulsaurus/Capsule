import AssetKit
import CapsuleUI
import ImagePipeline
import SwiftUI

/// Pages horizontally through the viewer's assets, one screen at a time.
///
/// The two platforms cannot share an implementation: `PageTabViewStyle` — the
/// swipe-driven `TabView` the iOS viewer is built on — does not exist on macOS,
/// and a plain macOS `TabView` would render one tab per asset, which is absurd
/// for a library-sized list. So the Mac gets the paging affordances it actually
/// has: arrow keys, and chevrons for pointer users.
///
/// Both variants drive the same `currentIndex` binding, so the slideshow timer
/// and the bottom bar in ``AssetViewerView`` are entirely platform-agnostic.
struct AssetPager: View {
    let assets: [Asset]
    @Binding var currentIndex: Int
    let mediaLoader: ViewerMediaLoader

    /// The slideshow advances the index from a timer, so the cross-fade has to
    /// be driven by the index changing rather than by a gesture.
    private static let pageAnimation: Animation = .easeInOut(duration: 0.4)

    var body: some View {
        #if os(iOS)
            TabView(selection: $currentIndex) {
                ForEach(Array(assets.enumerated()), id: \.element.id) { index, asset in
                    AssetPageView(asset: asset, mediaLoader: mediaLoader)
                        .tag(index)
                }
            }
            .tabViewStyle(.page(indexDisplayMode: .never))
            .ignoresSafeArea()
            .animation(Self.pageAnimation, value: currentIndex)
        #else
            ZStack {
                if assets.indices.contains(currentIndex) {
                    AssetPageView(asset: assets[currentIndex], mediaLoader: mediaLoader)
                        .id(assets[currentIndex].id)
                        .transition(.opacity)
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .animation(Self.pageAnimation, value: currentIndex)
            .overlay(alignment: .leading) { step(-1, symbol: "chevron.left", label: "app.viewer.previous_photo") }
            .overlay(alignment: .trailing) { step(1, symbol: "chevron.right", label: "app.viewer.next_photo") }
            .focusable()
            .focusEffectDisabled()
            .onKeyPress(.leftArrow) { advance(by: -1) ? .handled : .ignored }
            .onKeyPress(.rightArrow) { advance(by: 1) ? .handled : .ignored }
        #endif
    }

    #if !os(iOS)
        /// A pointer-driven paging control, disabled at the ends of the list so
        /// the affordance stays honest about where the user is.
        @ViewBuilder
        private func step(_ delta: Int, symbol: String, label: LocalizedStringKey) -> some View {
            Button {
                _ = advance(by: delta)
            } label: {
                Image(systemName: symbol)
                    .font(.title2)
                    .foregroundStyle(.white)
                    .padding(CapsuleTheme.Spacing.medium)
                    .capsuleGlass(in: Circle(), interactive: true)
            }
            .buttonStyle(.plain)
            .accessibilityLabel(label)
            .disabled(!assets.indices.contains(currentIndex + delta))
            .padding(CapsuleTheme.Spacing.large)
        }

        /// Move `delta` pages, reporting whether the move was possible so a key
        /// press at the end of the list falls through to the responder chain.
        private func advance(by delta: Int) -> Bool {
            let target = currentIndex + delta
            guard assets.indices.contains(target) else { return false }
            currentIndex = target
            return true
        }
    #endif
}
