import SwiftUI
import Testing

@testable import CapsuleUI

@Suite("Design tokens")
struct CapsuleThemeTests {
    @Test("the spacing scale is strictly increasing")
    func spacingIsMonotonic() {
        let scale = [
            CapsuleTheme.Spacing.xxSmall,
            CapsuleTheme.Spacing.xSmall,
            CapsuleTheme.Spacing.small,
            CapsuleTheme.Spacing.medium,
            CapsuleTheme.Spacing.large,
            CapsuleTheme.Spacing.xLarge,
            CapsuleTheme.Spacing.xxLarge,
        ]
        #expect(scale == scale.sorted())
        #expect(Set(scale).count == scale.count)
    }

    @Test("the radius scale is strictly increasing")
    func radiusIsMonotonic() {
        let scale = [
            CapsuleTheme.Radius.small,
            CapsuleTheme.Radius.card,
            CapsuleTheme.Radius.medium,
            CapsuleTheme.Radius.large,
        ]
        #expect(scale == scale.sorted())
        #expect(Set(scale).count == scale.count)
    }

    /// Every type role has to be a *distinct* font, or the scale is decoration.
    ///
    /// The failure this catches is a role added by copy-paste that resolves to
    /// the same font as the role above it — at which point two things the design
    /// says are different are drawn identically, and nobody notices until the
    /// two are next to each other on a screen.
    @Test("every type role resolves to a distinct font")
    func typeRolesAreDistinct() {
        let roles: [Font] = [
            CapsuleTheme.Typography.screenTitle,
            CapsuleTheme.Typography.sectionTitle,
            CapsuleTheme.Typography.cardTitle,
            CapsuleTheme.Typography.body,
            CapsuleTheme.Typography.rowLabel,
            CapsuleTheme.Typography.detail,
            CapsuleTheme.Typography.badge,
            CapsuleTheme.Typography.caption,
            CapsuleTheme.Typography.controlGlyph,
            CapsuleTheme.Typography.numeric,
        ]
        #expect(Set(roles).count == roles.count)
    }

    /// `Font.body` and friends scale with Dynamic Type; `Font.system(size:)`
    /// does not. Asserting the roles are *equal to* system text styles is what
    /// keeps a future "just make it 13pt" edit from silently opting the app out
    /// of accessibility sizing.
    @Test("the roles that name a text style are that text style")
    func rolesAreSystemTextStyles() {
        #expect(CapsuleTheme.Typography.body == Font.body)
        #expect(CapsuleTheme.Typography.rowLabel == Font.subheadline)
        #expect(CapsuleTheme.Typography.detail == Font.footnote)
        #expect(CapsuleTheme.Typography.caption == Font.caption)
        #expect(CapsuleTheme.Typography.sectionTitle == Font.headline)
        #expect(CapsuleTheme.Typography.controlGlyph == Font.title3)
    }
}

@Suite("Glass variants")
struct CapsuleGlassVariantTests {
    @Test("a plain variant resolves to its base glass")
    func plainVariantsResolveToBase() {
        #expect(CapsuleGlassVariant.regular.resolved(tint: nil, interactive: false) == .regular)
        #expect(CapsuleGlassVariant.clear.resolved(tint: nil, interactive: false) == .clear)
    }

    /// Tint before interactive, always.
    ///
    /// `Glass` builds by returning a new value from each modifier, so the two
    /// orders produce different values and only one of them is what the SDK
    /// documents. Pinning it here means the vocabulary applies them the same way
    /// on every surface instead of each call site guessing.
    @Test("tint is applied before interactivity")
    func tintPrecedesInteractive() {
        let resolved = CapsuleGlassVariant.regular.resolved(tint: .red, interactive: true)
        #expect(resolved == Glass.regular.tint(.red).interactive())
    }

    @Test("clear glass stays clear once tinted and made interactive")
    func clearStaysClear() {
        let resolved = CapsuleGlassVariant.clear.resolved(tint: .white, interactive: true)
        #expect(resolved == Glass.clear.tint(.white).interactive())
        #expect(resolved != Glass.regular.tint(.white).interactive())
    }
}
