import CapsuleDomain
import SwiftUI

// MARK: - SyncStateBadge

/// The state of one asset's journey to durability, as a single glyph.
///
/// Eight states share one badge because they answer one question — *can I rely
/// on this photo being safe?* — and answering it in eight different visual
/// languages would make the grid unreadable. What varies is the glyph, the role,
/// and whether the badge announces itself; the geometry never does.
///
/// Two of the states are the ones this design exists for. **Quarantined** and
/// **unreadable on this device** describe assets that are intact but withheld,
/// and the app's contract is that nothing is ever silently dropped — so they get
/// a badge rather than an absence. A photo that simply vanished from the grid
/// would be indistinguishable from data loss.
///
/// `durable` renders nothing at all. It is the overwhelmingly common state, and a
/// badge on every cell is not information.
public struct SyncStateBadge: View {
    /// Where the badge sits, which decides how it is tinted.
    public enum Surface: Sendable, Equatable {
        /// On chrome — a list row, a toolbar, an inspector.
        case chrome
        /// Over photo content, where hue cannot be relied on to carry meaning.
        case media
    }

    private let state: AssetSyncState
    private let surface: Surface

    public init(_ state: AssetSyncState, surface: Surface = .media) {
        self.state = state
        self.surface = surface
    }

    public var body: some View {
        if let symbol {
            Image(systemName: symbol)
                .font(.caption2.weight(.semibold))
                .foregroundStyle(tint)
                .padding(CapsuleTheme.Spacing.xxSmall)
                .background(scrim)
                // The glyph alone is meaningless to VoiceOver, and these states
                // are precisely the ones a screen-reader user most needs told.
                .accessibilityLabel(Text(accessibilityKey))
                .accessibilityAddTraits(state.needsUserAttention ? .isStaticText : [])
        }
    }

    // MARK: Presentation

    private var symbol: String? {
        switch state {
        case .durable: nil
        case .uploading: "arrow.up.circle"
        case .awaitingOriginal: "clock.badge.checkmark"
        case .quarantined: "exclamationmark.shield"
        case .unreadableOnThisDevice: "eye.slash"
        case .writtenByNewerVersion: "sparkles"
        case .fullResolutionUnavailable: "arrow.down.left.and.arrow.up.right.circle"
        }
    }

    private var tint: Color {
        switch surface {
        case .chrome: CapsuleTheme.StateColors.tint(for: state.role)
        case .media: CapsuleTheme.StateColors.tintOverMedia(for: state.role)
        }
    }

    /// A scrim, not glass. Glass samples what is behind it, and what is behind a
    /// cell badge is a photograph — so a glass badge over a bright sky is
    /// invisible exactly when it matters.
    @ViewBuilder
    private var scrim: some View {
        if surface == .media {
            Circle().fill(.black.opacity(0.35))
        }
    }

    private var accessibilityKey: LocalizedStringKey {
        switch state {
        case .durable: "ios.asset.sync.durable"
        case .uploading: "ios.asset.sync.uploading"
        case .awaitingOriginal: "ios.asset.sync.awaiting_original"
        case .quarantined: "ios.asset.sync.quarantined"
        case .unreadableOnThisDevice: "ios.asset.sync.unreadable"
        case .writtenByNewerVersion: "ios.asset.sync.newer_version"
        case .fullResolutionUnavailable: "ios.asset.sync.full_resolution_unavailable"
        }
    }
}

#Preview("Sync states over media") {
    let states: [AssetSyncState] = [
        .uploading(tier: .original, transferred: 40, total: 100),
        .awaitingOriginal(heldBy: nil),
        .quarantined(QuarantineID("preview-quarantine")),
        .unreadableOnThisDevice(.localBytesCorrupt),
        .writtenByNewerVersion(SchemaAhead(surface: .sidecarSchema, found: "2", maxKnown: "1")),
        .fullResolutionUnavailable(bestAvailable: .preview),
    ]
    return HStack(spacing: CapsuleTheme.Spacing.medium) {
        ForEach(Array(states.enumerated()), id: \.offset) { _, state in
            SyncStateBadge(state, surface: .media)
        }
    }
    .padding()
    .background(LinearGradient(colors: [.orange, .purple], startPoint: .top, endPoint: .bottom))
}
