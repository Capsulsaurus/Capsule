import SwiftUI

// The Capsule adaptation of Apple's Liquid Glass design language.
//
// The app deploys to iOS 18 but is built with the iOS 26 SDK, so every Liquid
// Glass API must be `#available(iOS 26.0, *)`-gated with a pre-26 material
// fallback. Centralising that here keeps feature code free of availability
// noise: call sites use `capsuleGlass(...)`, `CapsuleGlassContainer`, and the
// gated helpers below, and the right thing happens on each OS.
//
// Guidance baked in (from Apple's HIG): glass belongs on the navigation /
// control layer, never on photo content; apply it last in a modifier chain;
// group nearby glass in a `CapsuleGlassContainer` because glass cannot sample
// glass; and let the system honour Reduce Transparency / Increased Contrast.

// MARK: - Variant

/// Which Liquid Glass material a surface uses, with its pre-26 fallback.
public enum CapsuleGlassVariant: Sendable {
    /// The default adaptive glass — toolbars, bars, buttons, floating controls.
    case regular
    /// Highly transparent glass for small controls floating over bright media.
    case clear

    /// The `Material` used on iOS 18–25, where Liquid Glass is unavailable.
    var fallbackMaterial: Material {
        switch self {
        case .regular: .regularMaterial
        case .clear: .ultraThinMaterial
        }
    }
}

// MARK: - glassEffect

public extension View {
    /// Apply Liquid Glass to this view, falling back to a blur `Material` on
    /// iOS 18–25.
    ///
    /// - Parameters:
    ///   - variant: `.regular` (default) or `.clear` for controls over media.
    ///   - shape: the glass silhouette; defaults to a `Capsule`.
    ///   - tint: an optional semantic tint (use sparingly — it conveys meaning,
    ///     not decoration).
    ///   - interactive: whether the glass reacts to touch (buttons / controls).
    @ViewBuilder
    func capsuleGlass(
        _ variant: CapsuleGlassVariant = .regular,
        in shape: some Shape = Capsule(),
        tint: Color? = nil,
        interactive: Bool = false
    ) -> some View {
        if #available(iOS 26.0, *) {
            modifier(GlassEffectModifier(
                variant: variant, shape: shape, tint: tint, interactive: interactive
            ))
        } else {
            background(variant.fallbackMaterial, in: shape)
        }
    }
}

/// Bridges ``CapsuleGlassVariant`` to the iOS 26 `glassEffect(_:in:)` modifier.
///
/// Isolated in an `@available` modifier so the `Glass` type never appears in a
/// signature the iOS 18 compiler path has to resolve.
@available(iOS 26.0, *)
private struct GlassEffectModifier<S: Shape>: ViewModifier {
    let variant: CapsuleGlassVariant
    let shape: S
    let tint: Color?
    let interactive: Bool

    func body(content: Content) -> some View {
        content.glassEffect(resolvedGlass, in: shape)
    }

    private var resolvedGlass: Glass {
        var glass: Glass = (variant == .clear) ? .clear : .regular
        if let tint { glass = glass.tint(tint) }
        if interactive { glass = glass.interactive() }
        return glass
    }
}

// MARK: - Container

/// Groups nearby glass surfaces so they blend and morph as one, per Apple's
/// "glass cannot sample glass" rule. A transparent passthrough on iOS 18–25.
public struct CapsuleGlassContainer<Content: View>: View {
    private let spacing: CGFloat?
    private let content: Content

    public init(spacing: CGFloat? = nil, @ViewBuilder content: () -> Content) {
        self.spacing = spacing
        self.content = content()
    }

    public var body: some View {
        if #available(iOS 26.0, *) {
            GlassEffectContainer(spacing: spacing) { content }
        } else {
            content
        }
    }
}

public extension View {
    /// Associate this glass surface with siblings in a ``CapsuleGlassContainer``
    /// so they morph together during transitions. A no-op on iOS 18–25.
    @ViewBuilder
    func capsuleGlassID(_ id: some Hashable & Sendable, in namespace: Namespace.ID) -> some View {
        if #available(iOS 26.0, *) {
            glassEffectID(id, in: namespace)
        } else {
            self
        }
    }
}

// MARK: - Buttons

public extension View {
    /// Apply the Liquid Glass button style, falling back to `.bordered` /
    /// `.borderedProminent` on iOS 18–25.
    @ViewBuilder
    func capsuleGlassButtonStyle(prominent: Bool = false) -> some View {
        if #available(iOS 26.0, *) {
            if prominent {
                buttonStyle(.glassProminent)
            } else {
                buttonStyle(.glass)
            }
        } else {
            if prominent {
                buttonStyle(.borderedProminent)
            } else {
                buttonStyle(.bordered)
            }
        }
    }
}

// MARK: - Chrome behaviours (no-ops on iOS 18–25)

public extension View {
    /// Let the tab bar minimise as content scrolls down (iOS 26 Liquid Glass).
    @ViewBuilder
    func capsuleTabBarMinimizeOnScroll() -> some View {
        #if os(iOS)
        if #available(iOS 26.0, *) {
            tabBarMinimizeBehavior(.onScrollDown)
        } else {
            self
        }
        #else
        self
        #endif
    }

    /// Extend background content beneath the safe-area chrome (iOS 26).
    @ViewBuilder
    func capsuleBackgroundExtension() -> some View {
        if #available(iOS 26.0, *) {
            backgroundExtensionEffect()
        } else {
            self
        }
    }
}
