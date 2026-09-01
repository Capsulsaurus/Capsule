import SwiftUI

// The Capsule adaptation of Apple's Liquid Glass design language.
//
// The app's floor is iOS 26 / macOS 26, so every Liquid Glass API is simply
// available: there are no `#available` fences and no pre-26 material fallbacks
// anywhere in this file, and there must not be. What remains here is not a
// compatibility shim but a *vocabulary* — `capsuleGlass(...)`,
// `CapsuleGlassContainer`, `capsuleGlassButtonStyle(...)` — so the HIG rules
// below are applied consistently instead of being re-litigated per screen.
//
// Guidance baked in (from Apple's HIG):
//   * glass belongs on the navigation / control layer, never on photo content
//     (a scrim or a `.bar` material is the right answer over a photo);
//   * apply it last in a modifier chain, so it sees the finished shape;
//   * group nearby glass in a `CapsuleGlassContainer`, because glass cannot
//     sample glass;
//   * let the system honour Reduce Transparency / Increased Contrast — nothing
//     here second-guesses those settings.

// MARK: - Variant

/// Which Liquid Glass material a surface uses.
public enum CapsuleGlassVariant: Sendable {
    /// The default adaptive glass — toolbars, bars, buttons, floating controls.
    case regular
    /// Highly transparent glass for small controls floating over bright media.
    case clear

    /// The resolved `Glass` value, with the semantic tint and interactivity
    /// applied in the order the SDK expects.
    func resolved(tint: Color?, interactive: Bool) -> Glass {
        var glass: Glass = self == .clear ? .clear : .regular
        if let tint { glass = glass.tint(tint) }
        if interactive { glass = glass.interactive() }
        return glass
    }
}

// MARK: - glassEffect

public extension View {
    /// Apply Liquid Glass to this view.
    ///
    /// - Parameters:
    ///   - variant: `.regular` (default) or `.clear` for controls over media.
    ///   - shape: the glass silhouette; defaults to a `Capsule`.
    ///   - tint: an optional semantic tint (use sparingly — it conveys meaning,
    ///     not decoration).
    ///   - interactive: whether the glass reacts to touch (buttons / controls).
    func capsuleGlass(
        _ variant: CapsuleGlassVariant = .regular,
        in shape: some Shape = Capsule(),
        tint: Color? = nil,
        interactive: Bool = false
    ) -> some View {
        glassEffect(variant.resolved(tint: tint, interactive: interactive), in: shape)
    }
}

// MARK: - Container

/// Groups nearby glass surfaces so they blend and morph as one, per Apple's
/// "glass cannot sample glass" rule.
public struct CapsuleGlassContainer<Content: View>: View {
    private let spacing: CGFloat?
    private let content: Content

    public init(spacing: CGFloat? = nil, @ViewBuilder content: () -> Content) {
        self.spacing = spacing
        self.content = content()
    }

    public var body: some View {
        GlassEffectContainer(spacing: spacing) { content }
    }
}

public extension View {
    /// Associate this glass surface with siblings in a ``CapsuleGlassContainer``
    /// so they morph together during transitions.
    func capsuleGlassID(_ id: some Hashable & Sendable, in namespace: Namespace.ID) -> some View {
        glassEffectID(id, in: namespace)
    }
}

// MARK: - Buttons

public extension View {
    /// Apply the Liquid Glass button style.
    @ViewBuilder
    func capsuleGlassButtonStyle(prominent: Bool = false) -> some View {
        if prominent {
            buttonStyle(.glassProminent)
        } else {
            buttonStyle(.glass)
        }
    }
}

// MARK: - Chrome behaviours

public extension View {
    /// Let the tab bar minimise as content scrolls down.
    ///
    /// The `#if` is a *capability* branch, not a version one: macOS has no tab
    /// bar for the behaviour to apply to, so the modifier does not exist there.
    @ViewBuilder
    func capsuleTabBarMinimizeOnScroll() -> some View {
        #if os(iOS)
            tabBarMinimizeBehavior(.onScrollDown)
        #else
            self
        #endif
    }

    /// Extend background content beneath the safe-area chrome.
    func capsuleBackgroundExtension() -> some View {
        backgroundExtensionEffect()
    }
}

// MARK: - Scroll edges

public extension View {
    /// Let scrolling content dissolve under the bar at `edge` instead of being
    /// cut off by an opaque strip.
    ///
    /// This is the iOS 26 answer to the problem the app previously solved by
    /// hand: a `safeAreaInset` filled with `.background(.bar)`, which is an
    /// opaque band that content slides *behind* and vanishes at. The scroll edge
    /// effect instead fades and blurs the content into the bar, so the bar reads
    /// as floating over a continuous surface — the same relationship glass has
    /// with everything else it sits on.
    ///
    /// `.soft` is the default here rather than `.hard` because a hard edge draws
    /// a visible boundary line, which re-creates the band this exists to remove.
    func capsuleScrollEdgeEffect(
        _ style: ScrollEdgeEffectStyle = .soft,
        for edges: Edge.Set
    ) -> some View {
        scrollEdgeEffectStyle(style, for: edges)
    }
}

// MARK: - Search

public extension View {
    /// Let the search field collapse into the toolbar until it is reached for.
    ///
    /// The `#if` is a capability branch: macOS puts search in the window toolbar,
    /// where there is nothing to minimise into, so the modifier does not exist
    /// there.
    @ViewBuilder
    func capsuleSearchToolbarBehavior() -> some View {
        #if os(iOS)
            searchToolbarBehavior(.minimize)
        #else
            self
        #endif
    }
}

// MARK: - Zoom transitions

public extension View {
    /// Mark this view as the thing a zoom transition grows *out of*.
    ///
    /// Pair with ``capsuleZoomTransition(id:in:)`` on the presented view, using
    /// the same `id` and namespace. Without both halves the presentation falls
    /// back to a cross-fade — which is what the app did everywhere before this
    /// existed, despite the iOS 26 floor being justified partly by this API.
    func capsuleZoomSource(id: some Hashable, in namespace: Namespace.ID) -> some View {
        matchedTransitionSource(id: id, in: namespace)
    }

    /// Present this view by growing it from the matching
    /// ``capsuleZoomSource(id:in:)``, and shrink it back on dismissal.
    ///
    /// The `#if` is a capability branch: `.zoom` is unavailable on macOS, where
    /// a detail presentation is a window or a sheet rather than a view that
    /// takes over the screen, so there is no tile for it to grow from. The Mac
    /// keeps the system's own presentation animation.
    @ViewBuilder
    func capsuleZoomTransition(id: some Hashable, in namespace: Namespace.ID) -> some View {
        #if os(iOS)
            navigationTransition(.zoom(sourceID: id, in: namespace))
        #else
            self
        #endif
    }
}
