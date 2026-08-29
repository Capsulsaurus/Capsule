import SwiftUI

/// Cross-platform wrappers for the SwiftUI chrome modifiers that exist only
/// where there is a navigation bar, a status bar, or a full-screen presentation
/// — i.e. on iOS and iPadOS but not macOS.
///
/// Feature views call these unconditionally so no view body carries an
/// `#if os(...)`; the branch lives here once, next to the reason for it. The
/// pattern mirrors `capsuleTabBarMinimizeOnScroll()` in ``CapsuleGlass``.
///
/// These live in `CapsuleUI` because they are chrome rather than any one
/// feature's concern, and because the timeline and the viewer both need them.
/// The directory name is load-bearing: `.swiftlint.yml`'s `no_platform_ui_import`
/// rule exempts `Platform/` and `PlatformCollection/`, which is the same reason
/// the collection-view island lives where it does.
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
    func capsuleFullScreenCover<Item: Identifiable>(
        item: Binding<Item?>,
        @ViewBuilder content: @escaping (Item) -> some View
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

    /// A sheet that rests at `detents`, shows a grab handle, and lets whatever
    /// it covers stay visible behind it.
    ///
    /// The variant a sheet over *media* wants. ``capsuleSheetDetents()`` gives a
    /// sheet the system's opaque background, which is right over a list and
    /// wrong over a photograph: the photo is the context for everything the
    /// sheet says, so it has to remain on screen. The drag indicator is here for
    /// the same reason — a sheet the user is expected to resize has to look
    /// resizable, and a sheet over a photo has no navigation bar to hint it.
    ///
    /// On macOS this degrades to ``capsuleSheetDetents()``'s fixed panel: a Mac
    /// sheet is neither draggable nor detented, and faking either would be a
    /// worse answer than the platform's own.
    func capsuleMediaSheet(
        detents: Set<PresentationDetent>,
        selection: Binding<PresentationDetent>? = nil
    ) -> some View {
        #if os(iOS)
            modifier(CapsuleMediaSheetModifier(detents: detents, selection: selection))
        #else
            capsuleSheetDetents()
        #endif
    }
}

#if os(iOS)

    /// The presentation modifiers ``SwiftUI/View/capsuleMediaSheet(detents:selection:)``
    /// applies, in one place.
    ///
    /// A `ViewModifier` rather than an inline chain because `presentationDetents`
    /// has two spellings — with and without a selection binding — and branching
    /// between them inside a `some View` chain otherwise needs an `AnyView` or a
    /// duplicated body.
    private struct CapsuleMediaSheetModifier: ViewModifier {
        let detents: Set<PresentationDetent>
        let selection: Binding<PresentationDetent>?

        func body(content: Content) -> some View {
            applyDetents(to: content)
                .presentationDragIndicator(.visible)
                // `.thinMaterial` rather than an opaque background: the sheet is
                // over a photograph, and the photograph is the subject.
                .presentationBackground(.thinMaterial)
                .presentationCornerRadius(CapsuleTheme.Radius.large)
        }

        @ViewBuilder
        private func applyDetents(to content: Content) -> some View {
            if let selection {
                content.presentationDetents(detents, selection: selection)
            } else {
                content.presentationDetents(detents)
            }
        }
    }

#endif

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
