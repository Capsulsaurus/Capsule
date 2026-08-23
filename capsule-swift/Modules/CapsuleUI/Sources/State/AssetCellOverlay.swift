import CapsuleDomain
import CapsuleFoundation
import SwiftUI

// MARK: - AssetCellOverlay

/// Everything a grid cell says about its asset besides the picture itself.
///
/// One view rather than a scattering of modifiers, because the badges compete
/// for four corners of a tile that is often 90 points square, and the rules for
/// what wins are only expressible in one place. The corner assignment is fixed
/// and never varies by asset, so the eye learns it once:
///
/// | Corner | Says |
/// | --- | --- |
/// | top-leading | the culling flag, during a review pass |
/// | top-trailing | sync state, and whether the asset is hidden |
/// | bottom-leading | what kind of media it is — duration, Live Photo |
/// | bottom-trailing | how many assets the stack holds |
///
/// **Absence is the default.** A settled, unflagged, unstacked photo — which is
/// nearly all of them — shows nothing at all. A grid where every tile carries a
/// glyph has taught the user to ignore glyphs, which costs exactly the states
/// that matter: quarantined, unreadable, awaiting an original.
public struct AssetCellOverlay: View {
    private let asset: LibraryAsset
    private let stackMemberCount: Int?
    private let showsCullFlag: Bool

    /// - Parameters:
    ///   - stackMemberCount: how many assets the stack holds, when the caller
    ///     knows. `StackMembership` deliberately carries only *this* asset's
    ///     role and ordering — the size of a stack is a fact about the stack,
    ///     not about a member — so a grid that wants the number fetches it with
    ///     its section aggregate and passes it in. Absent, the badge falls back
    ///     to naming the *kind* of stack, which is the more useful half anyway.
    ///   - showsCullFlag: only true during a culling pass. Outside one, a
    ///     rejected photo is just a photo, and marking it in the ordinary
    ///     timeline would read as an error.
    public init(_ asset: LibraryAsset, stackMemberCount: Int? = nil, showsCullFlag: Bool = false) {
        self.asset = asset
        self.stackMemberCount = stackMemberCount
        self.showsCullFlag = showsCullFlag
    }

    public var body: some View {
        ZStack {
            corner(.topLeading) { cullBadge }
            corner(.topTrailing) { statusBadges }
            corner(.bottomLeading) { mediaBadge }
            corner(.bottomTrailing) { stackBadge }
        }
        .padding(CapsuleTheme.Spacing.xSmall)
        // The overlay describes the tile; the tile itself is the accessibility
        // element. Nested elements here would make a VoiceOver sweep of a grid
        // five times longer for no added meaning.
        .accessibilityHidden(true)
        .allowsHitTesting(false)
    }

    // MARK: Corners

    @ViewBuilder
    private func corner(
        _ alignment: Alignment,
        @ViewBuilder content: () -> some View
    ) -> some View {
        VStack {
            content()
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: alignment)
    }

    // MARK: Badges

    @ViewBuilder
    private var cullBadge: some View {
        if showsCullFlag, asset.cull != .neutral {
            Image(systemName: asset.cull == .pick ? "checkmark.circle.fill" : "xmark.circle.fill")
                .font(.caption2)
                .foregroundStyle(asset.cull.tint, .black.opacity(0.35))
        }
    }

    @ViewBuilder
    private var statusBadges: some View {
        HStack(spacing: CapsuleTheme.Spacing.xxSmall) {
            if asset.isUserHidden {
                glyph("eye.slash.fill")
            }
            SyncStateBadge(asset.syncState, surface: .media)
        }
    }

    @ViewBuilder
    private var mediaBadge: some View {
        switch asset.mediaType {
        case .photo:
            EmptyView()
        case .livePhoto:
            glyph("livephoto")
        case .video:
            if let duration = asset.durationMilliseconds {
                Text(Self.durationText(milliseconds: duration))
                    .font(.caption2.monospacedDigit().weight(.semibold))
                    .foregroundStyle(CapsuleTheme.Colors.onMedia)
                    .shadow(radius: 2)
            } else {
                glyph("play.fill")
            }
        }
    }

    @ViewBuilder
    private var stackBadge: some View {
        // Only the cover of a *collapsed* stack advertises the stack. A member
        // shown because the stack is expanded is being looked at individually,
        // and a badge on every sibling would be noise.
        if let membership = asset.stackMembership, membership.isStackCover {
            HStack(spacing: CapsuleTheme.Spacing.xxSmall) {
                Image(systemName: Self.symbol(for: membership.stackType))
                if let stackMemberCount, stackMemberCount > 1 {
                    Text(verbatim: "\(stackMemberCount)")
                        .monospacedDigit()
                }
            }
            .font(.caption2.weight(.semibold))
            .foregroundStyle(CapsuleTheme.Colors.onMedia)
            .padding(.horizontal, CapsuleTheme.Spacing.xSmall)
            .padding(.vertical, CapsuleTheme.Spacing.xxSmall)
            .background(Capsule().fill(.black.opacity(0.35)))
        }
    }

    // swiftlint:disable cyclomatic_complexity
    // Exhaustiveness is the point: `StackType` is a closed wire enum, and this
    // switch is what makes adding a kind without giving it a glyph a compile
    // error. Collapsing the branches into a lookup table to satisfy the rule
    // would trade that guarantee for a number.

    /// A stack's kind, as a glyph.
    ///
    /// An unrecognised kind still gets a badge rather than disappearing: a stack
    /// written by a newer client is a real stack, and silently rendering its
    /// cover as a lone photo would misrepresent the library.
    static func symbol(for stackType: StackType) -> String {
        switch stackType {
        case .rawJpeg: "camera.aperture"
        case .burst: "square.stack.3d.down.right"
        case .livePhoto: "livephoto"
        case .portrait: "f.cursive"
        case .smartSelection: "wand.and.stars"
        case .hdrBracket: "camera.filters"
        case .focusStack: "camera.metering.spot"
        case .pixelShift: "squareshape.split.3x3"
        case .panorama: "pano"
        case .proxy: "arrow.trianglehead.2.clockwise.rotate.90"
        case .chaptered: "list.bullet.rectangle"
        case .dualAudio: "waveform.badge.plus"
        case .custom: "square.stack"
        case .unknown: "square.stack"
        }
    }

    // swiftlint:enable cyclomatic_complexity

    private func glyph(_ systemName: String) -> some View {
        Image(systemName: systemName)
            .font(.caption2.weight(.semibold))
            .foregroundStyle(CapsuleTheme.Colors.onMedia)
            .shadow(radius: 2)
    }

    // MARK: Formatting

    /// `m:ss`, or `h:mm:ss` past an hour.
    ///
    /// Formatted arithmetically rather than through `DateComponentsFormatter`:
    /// this runs for every visible video tile on every frame of a scroll, and
    /// the formatter allocates.
    static func durationText(milliseconds: Int64) -> String {
        let totalSeconds = max(0, milliseconds / 1000)
        let seconds = totalSeconds % 60
        let minutes = (totalSeconds / 60) % 60
        let hours = totalSeconds / 3600
        if hours > 0 {
            return String(format: "%d:%02d:%02d", hours, minutes, seconds)
        }
        return String(format: "%d:%02d", minutes, seconds)
    }
}
