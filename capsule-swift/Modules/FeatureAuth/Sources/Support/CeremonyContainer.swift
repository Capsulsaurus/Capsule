import CapsuleFoundation
import CapsuleUI
import SwiftUI

// MARK: - CeremonyContainer

/// The shell every ceremony screen sits in.
///
/// One container because the platforms want genuinely different things and a
/// phone layout stretched across a 27-inch display is the classic tell of a
/// port rather than an app:
///
/// - **iPhone / iPad**: full-bleed, scrolling, the content taking the width it
///   is given.
/// - **Mac**: a comfortable, bounded column — a ceremony is a sheet-sized
///   task, not a document window, and a line of body text 1400 points wide is
///   unreadable regardless of how much space the display has.
///
/// The branch is on *capability* (``PlatformEnvironment/isTouchFirst``) rather
/// than on `#if os(...)`, so a future destination is a matter of extending that
/// type rather than auditing every screen.
public struct CeremonyContainer<Content: View>: View {
    /// The widest a column of body text is allowed to get.
    private static var readableWidth: CGFloat { 560 }
    /// The smallest a Mac sheet may be before its content starts fighting.
    private static var minimumWindowHeight: CGFloat { 520 }

    private let content: Content

    public init(@ViewBuilder content: () -> Content) {
        self.content = content()
    }

    public var body: some View {
        ScrollView {
            content
                .frame(maxWidth: Self.readableWidth, alignment: .leading)
                .frame(maxWidth: .infinity)
                .padding(.horizontal, CapsuleTheme.Spacing.large)
                .padding(.vertical, CapsuleTheme.Spacing.xLarge)
        }
        .frame(
            minWidth: PlatformEnvironment.isTouchFirst ? nil : Self.readableWidth,
            minHeight: PlatformEnvironment.isTouchFirst ? nil : Self.minimumWindowHeight
        )
        // A ceremony is a modal task: the scroll view owns the whole surface, so
        // its background must be the window's rather than a transparent hole.
        .background(.background)
    }
}

// MARK: - CeremonyHeader

/// A ceremony's title and one-sentence explanation.
///
/// Both are always present. A title alone leaves the user to infer what a step
/// costs them, and the sentence is the difference between "Generating device
/// keys" and "Making keys that never leave this device".
public struct CeremonyHeader: View {
    private let titleKey: LocalizedStringKey
    private let subtitleKey: LocalizedStringKey
    private let symbolName: String

    public init(titleKey: LocalizedStringKey, subtitleKey: LocalizedStringKey, symbolName: String) {
        self.titleKey = titleKey
        self.subtitleKey = subtitleKey
        self.symbolName = symbolName
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: CapsuleTheme.Spacing.small) {
            Image(systemName: symbolName)
                .font(.largeTitle)
                .foregroundStyle(.tint)
                .accessibilityHidden(true)
            Text(titleKey)
                .font(.largeTitle.weight(.semibold))
                .fixedSize(horizontal: false, vertical: true)
            Text(subtitleKey)
                .font(.body)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
    }
}
