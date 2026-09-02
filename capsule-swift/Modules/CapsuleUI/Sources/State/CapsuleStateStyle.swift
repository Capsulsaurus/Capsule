import CapsuleDomain
import SwiftUI

// MARK: - CapsuleStateRole

/// The semantic roles Capsule's own states are drawn in.
///
/// Roles rather than colours, for one concrete reason: contrast has to be proven,
/// and proving it per *colour* proves nothing — the same amber is fine on a
/// settings row and illegible over a photograph. A role names the meaning, the
/// style resolves it per surface, and the accessibility audit checks each role
/// once instead of each site.
///
/// The set is deliberately small. Every Capsule state maps onto one of these
/// five, which is what stops the app growing a private palette per feature.
public enum CapsuleStateRole: Sendable, Equatable, Hashable, CaseIterable {
    /// Settled and verified. The state most assets are in, most of the time.
    case settled
    /// Working — a transfer, a scan, a ceremony step. Resolves on its own.
    case inFlight
    /// Waiting on something outside this device. Also resolves on its own, but
    /// not soon, and not because of anything the user can do.
    case waiting
    /// Degraded but non-destructive: a lower representation tier, an
    /// unreachable origin. Nothing is lost and nothing is asked of the user.
    case degraded
    /// Held for a human decision. The only role that is genuinely actionable,
    /// which is why it is the only one drawn in the alarm colour.
    case attention
}

// MARK: - Mapping

public extension AssetSyncState {
    /// How this state should read.
    ///
    /// `writtenByNewerVersion` maps to ``CapsuleStateRole/attention`` rather
    /// than to `degraded` even though nothing is broken: the asset is intact and
    /// preserved verbatim, but this build will not *write* it, and a user who
    /// edits it in another client and loses the round trip was never told. The
    /// copy that goes with it has to say "created with a newer version of
    /// Capsule", never "damaged".
    var role: CapsuleStateRole {
        switch self {
        case .durable: .settled
        case .uploading: .inFlight
        case .awaitingOriginal: .waiting
        case .fullResolutionUnavailable: .degraded
        case .quarantined, .unreadableOnThisDevice, .writtenByNewerVersion: .attention
        }
    }
}

public extension CullFlag {
    /// Culling flags are their own vocabulary and deliberately do **not** reuse
    /// the sync roles: a rejected photo is not an error, and drawing it in the
    /// alarm colour would make a review pass look like a failure report.
    var tint: Color {
        switch self {
        case .pick: .green
        case .reject: .red
        case .neutral: .secondary
        case let .unknown(raw): raw.isEmpty ? .secondary : .orange
        }
    }
}

// MARK: - Palette

public extension CapsuleTheme {
    /// Colours for the state roles, on a chrome surface.
    enum StateColors {
        public static func tint(for role: CapsuleStateRole) -> Color {
            switch role {
            case .settled: .secondary
            case .inFlight: .accentColor
            case .waiting: .orange
            case .degraded: .yellow
            case .attention: .red
            }
        }

        /// The tint to use when the badge sits **over a photograph**.
        ///
        /// Photo content is arbitrary, so a hue that carries meaning against a
        /// settings background carries none against a sunset. Over media, every
        /// non-actionable role collapses to white-on-scrim and only
        /// ``CapsuleStateRole/attention`` keeps a hue — because it is the only
        /// one worth interrupting the picture for.
        public static func tintOverMedia(for role: CapsuleStateRole) -> Color {
            role == .attention ? .red : CapsuleTheme.Colors.onMedia
        }
    }
}
