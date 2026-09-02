import SwiftUI

// MARK: - MediaScrim

/// A dark capsule behind white text or a glyph that sits on a photograph.
///
/// A drop shadow is not enough. A shadow softens an edge against a *mid* tone
/// and does nothing at all against a bright one — a white duration badge on a
/// snow scene or a pale sky is unreadable however much it is shadowed, which is
/// precisely the case a photo library produces constantly. A scrim puts a known
/// backdrop behind the glyph, so its contrast stops depending on which
/// photograph it lands on.
///
/// Not glass: `CapsuleGlass`'s own rules say glass belongs on the navigation
/// layer and never over photo content, and glass over a photo would make the
/// legibility problem worse by sampling the very thing being competed with.
public extension View {
    /// Put this label on a scrim sized to it.
    func mediaScrim(
        horizontal: CGFloat = CapsuleTheme.Spacing.xSmall,
        vertical: CGFloat = CapsuleTheme.Spacing.xxSmall
    ) -> some View {
        padding(.horizontal, horizontal)
            .padding(.vertical, vertical)
            .background(.black.opacity(0.45), in: Capsule())
    }
}
