import SwiftUI

/// Cross-platform wrappers for the SwiftUI chrome modifiers that exist only
/// where there is a navigation bar, a status bar, or a full-screen presentation
/// — i.e. on iOS and iPadOS but not macOS.
///
/// Feature views call these unconditionally so no view body carries an
/// `#if os(...)`; the branch lives here once, next to the reason for it. The
/// pattern mirrors `capsuleTabBarMinimizeOnScroll()` in `CapsuleUI`.
///
/// These live in `FeatureViewer` because it is the one module every other
/// feature module already depends on. They are chrome, not viewer concerns, and
/// belong in `CapsuleUI` as soon as that module owns cross-platform chrome —
/// moving them is a rename of the import, nothing more.
public extension View {
    /// Render the navigation title inline rather than large.
    ///
    /// macOS window titles have no large/inline distinction, so this is a no-op
    /// there rather than an unavailable-API compile error.
    func capsuleNavigationBarInline() -> some View {
        #if os(iOS)
            navigationBarTitleDisplayMode(.inline)
        #else
            self
        #endif
    }

    /// Hide the status bar for an immersive, edge-to-edge presentation.
    ///
    /// macOS has a menu bar rather than a status bar, and an app does not get to
    /// hide it from a view, so this is a no-op there.
    func capsuleStatusBarHidden(_ hidden: Bool = true) -> some View {
        #if os(iOS)
            statusBarHidden(hidden)
        #else
            self
        #endif
    }

    /// Present `content` over the entire interface, keyed off an optional item.
    ///
    /// `fullScreenCover` does not exist on macOS — a Mac window is already the
    /// full presentation surface — so this maps to a `sheet` there, given a
    /// minimum size large enough that a photo viewer is actually usable instead
    /// of the default, form-sized sheet.
    func capsuleFullScreenCover<Item: Identifiable, Content: View>(
        item: Binding<Item?>,
        @ViewBuilder content: @escaping (Item) -> Content
    ) -> some View {
        #if os(iOS)
            fullScreenCover(item: item, content: content)
        #else
            sheet(item: item) { value in
                content(value)
                    .frame(
                        minWidth: CapsuleSheetMetrics.coverMinWidth,
                        minHeight: CapsuleSheetMetrics.coverMinHeight
                    )
            }
        #endif
    }

    /// Let a sheet rest at half height and expand to full.
    ///
    /// Detents are a touch-sheet affordance and are unavailable on macOS, where
    /// a sheet is a fixed-size panel instead; there this pins a sensible minimum
    /// size so the panel is not collapsed to its content's intrinsic width.
    func capsuleSheetDetents() -> some View {
        #if os(iOS)
            presentationDetents([.medium, .large])
        #else
            frame(
                minWidth: CapsuleSheetMetrics.panelMinWidth,
                minHeight: CapsuleSheetMetrics.panelMinHeight
            )
        #endif
    }
}

#if !os(iOS)

    /// Sizes for the macOS sheets that stand in for iOS full-screen and detented
    /// presentations. Named rather than inlined so the two call sites above stay
    /// readable and the numbers are auditable in one place.
    enum CapsuleSheetMetrics {
        static let coverMinWidth: CGFloat = 960
        static let coverMinHeight: CGFloat = 640
        static let panelMinWidth: CGFloat = 420
        static let panelMinHeight: CGFloat = 540
    }

#endif
